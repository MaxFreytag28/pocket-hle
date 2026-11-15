/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `NSKeyedArchiver` and serialization of its object graph format.
//!
//! This is the counterpart of [super::ns_keyed_unarchiver], see there for
//! resources about the format. Archives are always written as binary plists.
//!
//! Foundation's own classes (strings, numbers, collections, data, dates) are
//! encoded directly by the archiver. Any other object is sent
//! `encodeWithCoder:`.

use super::ns_string::{get_static_str, to_rust_string};
use super::ns_value::NSNumberHostObject;
use super::{NSInteger, NSTimeInterval, NSUInteger};
use crate::frameworks::core_graphics::{CGPoint, CGRect, CGSize};
use crate::frameworks::uikit::ui_geometry::{
    NSStringFromCGPoint, NSStringFromCGRect, NSStringFromCGSize,
};
use crate::mem::{ConstPtr, ConstVoidPtr, GuestUSize, MutVoidPtr};
use crate::objc::{
    id, msg, msg_class, nil, objc_classes, release, retain, Class, ClassExports, HostObject,
    NSZonePtr,
};
use crate::Environment;
use plist::{Dictionary, Uid, Value};
use std::collections::HashMap;

struct NSKeyedArchiverHostObject {
    /// The `$objects` array. Index 0 is always `$null`.
    objects: Vec<Value>,
    /// Objects that were already encoded, and their index in `objects`.
    uids: HashMap<id, u64>,
    /// Classes that already have an entry in `objects`, by name.
    class_uids: HashMap<String, u64>,
    /// The `$top` dictionary.
    top: Dictionary,
    /// Dictionaries of the objects currently in `encodeWithCoder:`, innermost
    /// last. Keyed values go to the last of these, or to `top` if empty.
    scopes: Vec<Dictionary>,
    /// `NSMutableData*` to append the archive to in `finishEncoding`, if any.
    output: id,
    finished: bool,
}
impl HostObject for NSKeyedArchiverHostObject {}

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

@implementation NSKeyedArchiver: NSCoder

+ (id)allocWithZone:(NSZonePtr)_zone {
    let archiver = Box::new(NSKeyedArchiverHostObject {
        objects: vec![Value::String("$null".to_string())],
        uids: HashMap::new(),
        class_uids: HashMap::new(),
        top: Dictionary::new(),
        scopes: Vec::new(),
        output: nil,
        finished: false,
    });
    env.objc.alloc_object(this, archiver, &mut env.mem)
}

+ (id)archivedDataWithRootObject:(id)root {
    let archiver: id = msg![env; this alloc];
    let archiver: id = msg![env; archiver init];
    let root_key = get_static_str(env, super::ns_keyed_unarchiver::NSKeyedArchiveRootObjectKey);
    () = msg![env; archiver encodeObject:root forKey:root_key];
    let bytes = serialize(env, archiver);
    release(env, archiver);

    let len: GuestUSize = bytes.len().try_into().unwrap();
    let guest_bytes: MutVoidPtr = env.mem.alloc(len);
    env.mem.bytes_at_mut(guest_bytes.cast(), len).copy_from_slice(&bytes);
    msg_class![env; NSData dataWithBytesNoCopy:guest_bytes length:len]
}

+ (bool)archiveRootObject:(id)root
                   toFile:(id)path { // NSString *
    let data: id = msg![env; this archivedDataWithRootObject:root];
    msg![env; data writeToFile:path atomically:true]
}

- (id)initForWritingWithMutableData:(id)data { // NSMutableData *
    retain(env, data);
    env.objc.borrow_mut::<NSKeyedArchiverHostObject>(this).output = data;
    this
}

- (())finishEncoding {
    let host_obj = env.objc.borrow_mut::<NSKeyedArchiverHostObject>(this);
    if host_obj.finished {
        return;
    }
    host_obj.finished = true;
    let output = host_obj.output;
    if output == nil {
        return;
    }
    let bytes = serialize(env, this);
    let len: GuestUSize = bytes.len().try_into().unwrap();
    let guest_bytes: MutVoidPtr = env.mem.alloc(len);
    env.mem.bytes_at_mut(guest_bytes.cast(), len).copy_from_slice(&bytes);
    let guest_bytes_const: ConstPtr<u8> = guest_bytes.cast().cast_const();
    () = msg![env; output appendBytes:guest_bytes_const length:len];
    env.mem.free(guest_bytes);
}

- (())dealloc {
    let output = env.objc.borrow::<NSKeyedArchiverHostObject>(this).output;
    release(env, output);
    env.objc.dealloc_object(this, &mut env.mem)
}

- (bool)allowsKeyedCoding {
    true
}

- (())encodeObject:(id)object
            forKey:(id)key { // NSString *
    let uid = encode_object(env, this, object);
    set_value(env, this, key, Value::Uid(uid));
}
- (())encodeConditionalObject:(id)object
                       forKey:(id)key { // NSString *
    // TODO: this should only encode a reference if the object is encoded
    // unconditionally somewhere else in the archive.
    let uid = encode_object(env, this, object);
    set_value(env, this, key, Value::Uid(uid));
}

- (())encodeBool:(bool)value
          forKey:(id)key { // NSString *
    set_value(env, this, key, Value::Boolean(value));
}
- (())encodeInt:(i32)value
         forKey:(id)key { // NSString *
    set_value(env, this, key, Value::Integer(i64::from(value).into()));
}
- (())encodeInt32:(i32)value
           forKey:(id)key { // NSString *
    set_value(env, this, key, Value::Integer(i64::from(value).into()));
}
- (())encodeInt64:(i64)value
           forKey:(id)key { // NSString *
    set_value(env, this, key, Value::Integer(value.into()));
}
- (())encodeInteger:(NSInteger)value
             forKey:(id)key { // NSString *
    set_value(env, this, key, Value::Integer(i64::from(value).into()));
}
- (())encodeFloat:(f32)value
           forKey:(id)key { // NSString *
    set_value(env, this, key, Value::Real(value.into()));
}
- (())encodeDouble:(f64)value
            forKey:(id)key { // NSString *
    set_value(env, this, key, Value::Real(value));
}
- (())encodeBytes:(ConstPtr<u8>)bytes
           length:(NSUInteger)length
           forKey:(id)key { // NSString *
    let bytes = env.mem.bytes_at(bytes, length).to_vec();
    set_value(env, this, key, Value::Data(bytes));
}

// These come from a category in UIKit's UIGeometry.h
- (())encodeCGPoint:(CGPoint)point
             forKey:(id)key { // NSString *
    let string = NSStringFromCGPoint(env, point);
    () = msg![env; this encodeObject:string forKey:key];
}
- (())encodeCGSize:(CGSize)size
            forKey:(id)key { // NSString *
    let string = NSStringFromCGSize(env, size);
    () = msg![env; this encodeObject:string forKey:key];
}
- (())encodeCGRect:(CGRect)rect
            forKey:(id)key { // NSString *
    let string = NSStringFromCGRect(env, rect);
    () = msg![env; this encodeObject:string forKey:key];
}

@end

};

fn borrow_host_obj(env: &mut Environment, archiver: id) -> &mut NSKeyedArchiverHostObject {
    env.objc.borrow_mut(archiver)
}

/// Store a keyed value in the object currently being encoded.
fn set_value(env: &mut Environment, archiver: id, key: id, value: Value) {
    let key = to_rust_string(env, key).to_string();
    let host_obj = borrow_host_obj(env, archiver);
    match host_obj.scopes.last_mut() {
        Some(scope) => scope.insert(key, value),
        None => host_obj.top.insert(key, value),
    };
}

/// Get the uid of the entry describing a class, creating it if needed.
fn class_uid(env: &mut Environment, archiver: id, class_name: &str) -> Uid {
    let host_obj = borrow_host_obj(env, archiver);
    if let Some(&uid) = host_obj.class_uids.get(class_name) {
        return Uid::new(uid);
    }
    let mut dict = Dictionary::new();
    dict.insert(
        "$classname".to_string(),
        Value::String(class_name.to_string()),
    );
    // TODO: this should be the full superclass chain.
    let mut classes = vec![Value::String(class_name.to_string())];
    if class_name != "NSObject" {
        classes.push(Value::String("NSObject".to_string()));
    }
    dict.insert("$classes".to_string(), Value::Array(classes));
    let uid = host_obj.objects.len() as u64;
    host_obj.objects.push(Value::Dictionary(dict));
    host_obj.class_uids.insert(class_name.to_string(), uid);
    Uid::new(uid)
}

/// The core of the implementation: add an object to the archive (unless it is
/// already there) and return its uid.
///
/// This is recursive in practice: the `encodeWithCoder:` messages sent by this
/// function will be received by objects which will then send
/// `encodeXXX:forKey:` messages back to the archiver.
fn encode_object(env: &mut Environment, archiver: id, object: id) -> Uid {
    if object == nil {
        return Uid::new(0);
    }
    let host_obj = borrow_host_obj(env, archiver);
    if let Some(&uid) = host_obj.uids.get(&object) {
        return Uid::new(uid);
    }
    // Reserve the slot now: the object graph may contain cycles, and objects
    // are expected to come before the things they reference.
    let uid = host_obj.objects.len() as u64;
    host_obj.objects.push(Value::String("$null".to_string()));
    host_obj.uids.insert(object, uid);

    let class: Class = msg![env; object class];
    let is_kind_of = |env: &mut Environment, name: &str| {
        let other = env.objc.get_known_class(name, &mut env.mem);
        env.objc.class_is_subclass_of(class, other)
    };

    let value = if is_kind_of(env, "NSString") {
        Value::String(to_rust_string(env, object).to_string())
    } else if is_kind_of(env, "NSNumber") {
        match *env.objc.borrow::<NSNumberHostObject>(object) {
            NSNumberHostObject::Bool(x) => Value::Boolean(x),
            NSNumberHostObject::UnsignedLongLong(x) => Value::Integer(x.into()),
            NSNumberHostObject::UnsignedInt(x) => Value::Integer(u64::from(x).into()),
            NSNumberHostObject::Int(x) => Value::Integer(i64::from(x).into()),
            NSNumberHostObject::LongLong(x) => Value::Integer(x.into()),
            NSNumberHostObject::Short(x) => Value::Integer(i64::from(x).into()),
            NSNumberHostObject::Char(x) => Value::Integer(i64::from(x).into()),
            NSNumberHostObject::Float(x) => Value::Real(x.into()),
            NSNumberHostObject::Double(x) => Value::Real(x),
        }
    } else if is_kind_of(env, "NSArray") {
        let count: NSUInteger = msg![env; object count];
        let mut uids = Vec::new();
        for i in 0..count {
            let item: id = msg![env; object objectAtIndex:i];
            uids.push(Value::Uid(encode_object(env, archiver, item)));
        }
        let class_name = if is_kind_of(env, "NSMutableArray") {
            "NSMutableArray"
        } else {
            "NSArray"
        };
        let mut dict = Dictionary::new();
        dict.insert("NS.objects".to_string(), Value::Array(uids));
        let class_uid = class_uid(env, archiver, class_name);
        dict.insert("$class".to_string(), Value::Uid(class_uid));
        Value::Dictionary(dict)
    } else if is_kind_of(env, "NSDictionary") {
        let keys: id = msg![env; object allKeys];
        let count: NSUInteger = msg![env; keys count];
        let mut key_uids = Vec::new();
        let mut value_uids = Vec::new();
        for i in 0..count {
            let key: id = msg![env; keys objectAtIndex:i];
            let value: id = msg![env; object objectForKey:key];
            key_uids.push(Value::Uid(encode_object(env, archiver, key)));
            value_uids.push(Value::Uid(encode_object(env, archiver, value)));
        }
        let mut dict = Dictionary::new();
        dict.insert("NS.keys".to_string(), Value::Array(key_uids));
        dict.insert("NS.objects".to_string(), Value::Array(value_uids));
        // Our unarchiver can only create mutable dictionaries so far.
        let class_uid = class_uid(env, archiver, "NSMutableDictionary");
        dict.insert("$class".to_string(), Value::Uid(class_uid));
        Value::Dictionary(dict)
    } else if is_kind_of(env, "NSData") {
        let length: NSUInteger = msg![env; object length];
        let bytes: ConstVoidPtr = msg![env; object bytes];
        let bytes = if length == 0 {
            Vec::new()
        } else {
            env.mem.bytes_at(bytes.cast(), length).to_vec()
        };
        let class_name = if is_kind_of(env, "NSMutableData") {
            "NSMutableData"
        } else {
            "NSData"
        };
        let mut dict = Dictionary::new();
        dict.insert("NS.data".to_string(), Value::Data(bytes));
        let class_uid = class_uid(env, archiver, class_name);
        dict.insert("$class".to_string(), Value::Uid(class_uid));
        Value::Dictionary(dict)
    } else if is_kind_of(env, "NSDate") {
        let time: NSTimeInterval = msg![env; object timeIntervalSinceReferenceDate];
        let mut dict = Dictionary::new();
        dict.insert("NS.time".to_string(), Value::Real(time));
        let class_uid = class_uid(env, archiver, "NSDate");
        dict.insert("$class".to_string(), Value::Uid(class_uid));
        Value::Dictionary(dict)
    } else {
        borrow_host_obj(env, archiver).scopes.push(Dictionary::new());
        () = msg![env; object encodeWithCoder:archiver];
        let mut dict = borrow_host_obj(env, archiver).scopes.pop().unwrap();
        let class_name = env.objc.get_class_name(class).to_string();
        let class_uid = class_uid(env, archiver, &class_name);
        dict.insert("$class".to_string(), Value::Uid(class_uid));
        Value::Dictionary(dict)
    };

    borrow_host_obj(env, archiver).objects[uid as usize] = value;
    Uid::new(uid)
}

/// Produce the binary plist for everything encoded so far.
fn serialize(env: &mut Environment, archiver: id) -> Vec<u8> {
    let host_obj = borrow_host_obj(env, archiver);
    let mut root = Dictionary::new();
    root.insert("$version".to_string(), Value::Integer(100000u64.into()));
    root.insert(
        "$archiver".to_string(),
        Value::String("NSKeyedArchiver".to_string()),
    );
    root.insert("$top".to_string(), Value::Dictionary(host_obj.top.clone()));
    root.insert(
        "$objects".to_string(),
        Value::Array(host_obj.objects.clone()),
    );
    let mut bytes = Vec::new();
    Value::Dictionary(root)
        .to_writer_binary(&mut bytes)
        .unwrap();
    bytes
}
