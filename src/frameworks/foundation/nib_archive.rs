/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! Reading of the "NIBArchive" format.
//!
//! From iPhone OS 3.2 on, nib files compiled by Xcode are usually not keyed
//! archives in a binary plist any more, but use this more compact format. It
//! holds the same kind of object graph, so rather than teaching
//! `NSKeyedUnarchiver` a second format, this module converts a NIBArchive to
//! the plist structure of a keyed archive.
//!
//! The format, as far as it is needed here:
//! - A header: the magic string, two version numbers, then count and file
//!   offset for each of four tables: objects, keys, values and class names.
//!   All of these are little-endian 32-bit integers.
//! - Objects are a class name index and a range of values (first, count).
//! - Keys are length-prefixed strings.
//! - Values are a key index, a type byte and data depending on the type.
//! - Class names are length-prefixed strings with a list of fallback classes.
//! - Variable-length integers have 7 bits per byte, least significant first;
//!   the high bit is set on the *last* byte.
//!
//! Differences to keyed archives that are smoothed over here:
//! - Strings and numbers are objects (with `NS.bytes`, `NS.intval` etc).
//! - Collections have their members "inlined": as values that all have the
//!   key `UINibEncoderEmptyKey`.
//! - The root object takes the place of the `$top` dictionary.
//!
//! Points, sizes and rectangles remain as they are, as raw data rather than
//! strings: see `decode_floats` in the unarchiver.

use plist::{Dictionary, Uid, Value};

const MAGIC: &[u8] = b"NIBArchive";
const INLINED_KEY: &str = "UINibEncoderEmptyKey";

pub fn is_nib_archive(bytes: &[u8]) -> bool {
    bytes.starts_with(MAGIC)
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}
impl Reader<'_> {
    fn u8(&mut self) -> Option<u8> {
        let byte = *self.bytes.get(self.pos)?;
        self.pos += 1;
        Some(byte)
    }
    fn take(&mut self, len: usize) -> Option<&[u8]> {
        let slice = self.bytes.get(self.pos..self.pos.checked_add(len)?)?;
        self.pos += len;
        Some(slice)
    }
    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn varint(&mut self) -> Option<usize> {
        let mut result = 0usize;
        let mut shift = 0;
        loop {
            let byte = self.u8()?;
            result |= ((byte & 0x7f) as usize).checked_shl(shift)?;
            if byte & 0x80 != 0 {
                return Some(result);
            }
            shift += 7;
        }
    }
}

enum NibValue {
    Int(i64),
    Bool(bool),
    Real(f64),
    Data(Vec<u8>),
    Nil,
    Object(u32),
}

/// Convert a NIBArchive to the plist of an equivalent keyed archive.
/// Returns [None] if the data is malformed.
pub fn to_keyed_archive(bytes: &[u8]) -> Option<Dictionary> {
    let mut header = Reader {
        bytes,
        pos: MAGIC.len(),
    };
    let _format_version = header.u32()?;
    let _coder_version = header.u32()?;
    let object_count = header.u32()? as usize;
    let object_offset = header.u32()? as usize;
    let key_count = header.u32()? as usize;
    let key_offset = header.u32()? as usize;
    let value_count = header.u32()? as usize;
    let value_offset = header.u32()? as usize;
    let class_count = header.u32()? as usize;
    let class_offset = header.u32()? as usize;

    let mut reader = Reader {
        bytes,
        pos: key_offset,
    };
    let mut keys = Vec::with_capacity(key_count);
    for _ in 0..key_count {
        let len = reader.varint()?;
        keys.push(String::from_utf8_lossy(reader.take(len)?).into_owned());
    }

    reader.pos = class_offset;
    let mut classes = Vec::with_capacity(class_count);
    for _ in 0..class_count {
        let len = reader.varint()?;
        let fallback_count = reader.varint()?;
        reader.take(fallback_count.checked_mul(4)?)?;
        let name = reader.take(len)?;
        let name = name.strip_suffix(&[0]).unwrap_or(name);
        classes.push(String::from_utf8_lossy(name).into_owned());
    }

    reader.pos = value_offset;
    let mut values = Vec::with_capacity(value_count);
    for _ in 0..value_count {
        let key = reader.varint()?;
        let value = match reader.u8()? {
            0 => NibValue::Int(reader.u8()? as i8 as i64),
            1 => NibValue::Int(i16::from_le_bytes(reader.take(2)?.try_into().unwrap()) as i64),
            2 => NibValue::Int(i32::from_le_bytes(reader.take(4)?.try_into().unwrap()) as i64),
            3 => NibValue::Int(i64::from_le_bytes(reader.take(8)?.try_into().unwrap())),
            4 => NibValue::Bool(true),
            5 => NibValue::Bool(false),
            6 => NibValue::Real(f32::from_le_bytes(reader.take(4)?.try_into().unwrap()) as f64),
            7 => NibValue::Real(f64::from_le_bytes(reader.take(8)?.try_into().unwrap())),
            8 => {
                let len = reader.varint()?;
                NibValue::Data(reader.take(len)?.to_vec())
            }
            9 => NibValue::Nil,
            10 => NibValue::Object(reader.u32()?),
            _ => return None,
        };
        values.push((keys.get(key)?.as_str(), value));
    }

    // In the result, entry 0 is "$null", then come the objects (so object n
    // has uid n + 1), then one entry per class.
    let object_uid = |index: u32| Value::Uid(Uid::new(index as u64 + 1));
    let class_uid = |index: usize| Value::Uid(Uid::new((1 + object_count + index) as u64));
    let convert = |value: &NibValue| match value {
        NibValue::Int(int) => Value::Integer((*int).into()),
        NibValue::Bool(boolean) => Value::Boolean(*boolean),
        NibValue::Real(real) => Value::Real(*real),
        NibValue::Data(data) => Value::Data(data.clone()),
        NibValue::Nil => Value::Uid(Uid::new(0)),
        NibValue::Object(index) => object_uid(*index),
    };

    let mut objects = vec![Value::String(String::from("$null"))];
    let mut top = Dictionary::new();
    reader.pos = object_offset;
    for index in 0..object_count {
        let class = reader.varint()?;
        let first = reader.varint()?;
        let count = reader.varint()?;
        let class_name = classes.get(class)?.as_str();
        let values = values.get(first..first.checked_add(count)?)?;
        let find = |wanted: &str| values.iter().find(|(key, _)| *key == wanted).map(|(_, v)| v);
        let inlined = || {
            values
                .iter()
                .filter(|(key, _)| *key == INLINED_KEY)
                .map(|(_, value)| convert(value))
        };

        let object = match class_name {
            "NSString" | "NSMutableString" => match find("NS.bytes") {
                Some(NibValue::Data(data)) => {
                    Value::String(String::from_utf8_lossy(data).into_owned())
                }
                _ => Value::String(String::new()),
            },
            "NSNumber" => match (find("NS.intval"), find("NS.dblval"), find("NS.boolval")) {
                (Some(value), _, _) | (_, Some(value), _) | (_, _, Some(value)) => convert(value),
                _ => Value::Integer(0.into()),
            },
            "NSArray" | "NSMutableArray" | "NSSet" | "NSMutableSet" => {
                let mut dict = Dictionary::new();
                dict.insert(String::from("$class"), class_uid(class));
                dict.insert(String::from("NS.objects"), Value::Array(inlined().collect()));
                Value::Dictionary(dict)
            }
            "NSDictionary" | "NSMutableDictionary" => {
                // Keys and objects alternate.
                let members: Vec<Value> = inlined().collect();
                let mut dict = Dictionary::new();
                dict.insert(String::from("$class"), class_uid(class));
                dict.insert(
                    String::from("NS.keys"),
                    Value::Array(members.iter().step_by(2).cloned().collect()),
                );
                dict.insert(
                    String::from("NS.objects"),
                    Value::Array(members.iter().skip(1).step_by(2).cloned().collect()),
                );
                Value::Dictionary(dict)
            }
            _ => {
                let mut dict = Dictionary::new();
                dict.insert(String::from("$class"), class_uid(class));
                for (key, value) in values {
                    dict.insert(key.to_string(), convert(value));
                }
                if index == 0 {
                    top = dict.clone();
                    top.remove("$class");
                }
                Value::Dictionary(dict)
            }
        };
        objects.push(object);
    }
    for class_name in classes {
        let mut dict = Dictionary::new();
        dict.insert(String::from("$classname"), Value::String(class_name));
        objects.push(Value::Dictionary(dict));
    }

    let mut plist = Dictionary::new();
    plist.insert(String::from("$version"), Value::Integer(100000.into()));
    plist.insert(
        String::from("$archiver"),
        Value::String(String::from("NSKeyedArchiver")),
    );
    plist.insert(String::from("$top"), Value::Dictionary(top));
    plist.insert(String::from("$objects"), Value::Array(objects));
    Some(plist)
}
