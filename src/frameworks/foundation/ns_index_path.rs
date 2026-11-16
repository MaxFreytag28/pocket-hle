/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `NSIndexPath`, including UIKit's additions for table views (`row`,
//! `section`).

use super::ns_string::from_rust_string;
use super::{
    NSComparisonResult, NSInteger, NSOrderedAscending, NSOrderedDescending, NSOrderedSame,
    NSUInteger,
};
use crate::mem::{ConstPtr, MutPtr};
use crate::objc::{
    autorelease, id, msg, nil, objc_classes, retain, ClassExports, HostObject, NSZonePtr,
};
use crate::Environment;

#[derive(Default)]
struct NSIndexPathHostObject {
    indexes: Vec<NSUInteger>,
}
impl HostObject for NSIndexPathHostObject {}

/// Make a new index path (autoreleased).
pub fn index_path(env: &mut Environment, indexes: Vec<NSUInteger>) -> id {
    let class = env.objc.get_known_class("NSIndexPath", &mut env.mem);
    let host_object = Box::new(NSIndexPathHostObject { indexes });
    let new = env.objc.alloc_object(class, host_object, &mut env.mem);
    autorelease(env, new)
}

/// The indexes of an index path.
pub fn indexes(env: &Environment, path: id) -> &[NSUInteger] {
    if path == nil {
        return &[];
    }
    &env.objc.borrow::<NSIndexPathHostObject>(path).indexes
}

/// Section and row of an index path made for a table view.
pub fn section_and_row(env: &Environment, path: id) -> (NSUInteger, NSUInteger) {
    let indexes = indexes(env, path);
    (
        indexes.first().copied().unwrap_or(0),
        indexes.get(1).copied().unwrap_or(0),
    )
}

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

@implementation NSIndexPath: NSObject

+ (id)allocWithZone:(NSZonePtr)_zone {
    let host_object = Box::<NSIndexPathHostObject>::default();
    env.objc.alloc_object(this, host_object, &mut env.mem)
}

+ (id)indexPathWithIndex:(NSUInteger)index {
    index_path(env, vec![index])
}
+ (id)indexPathWithIndexes:(ConstPtr<NSUInteger>)indexes length:(NSUInteger)length {
    let indexes = (0..length).map(|i| env.mem.read(indexes + i)).collect();
    index_path(env, indexes)
}
+ (id)indexPathForRow:(NSUInteger)row inSection:(NSUInteger)section {
    index_path(env, vec![section, row])
}

- (id)initWithIndex:(NSUInteger)index {
    env.objc.borrow_mut::<NSIndexPathHostObject>(this).indexes = vec![index];
    this
}
- (id)initWithIndexes:(ConstPtr<NSUInteger>)indexes length:(NSUInteger)length {
    let indexes = (0..length).map(|i| env.mem.read(indexes + i)).collect();
    env.objc.borrow_mut::<NSIndexPathHostObject>(this).indexes = indexes;
    this
}

// Index paths are immutable.
- (id)copyWithZone:(NSZonePtr)_zone {
    retain(env, this)
}

- (NSUInteger)length {
    env.objc.borrow::<NSIndexPathHostObject>(this).indexes.len() as NSUInteger
}
- (NSUInteger)indexAtPosition:(NSUInteger)position {
    let indexes = &env.objc.borrow::<NSIndexPathHostObject>(this).indexes;
    // Out of range gives NSNotFound.
    indexes.get(position as usize).copied().unwrap_or(NSInteger::MAX as NSUInteger)
}
- (())getIndexes:(MutPtr<NSUInteger>)out {
    let indexes = env.objc.borrow::<NSIndexPathHostObject>(this).indexes.clone();
    for (i, index) in indexes.into_iter().enumerate() {
        env.mem.write(out + i as NSUInteger, index);
    }
}
- (id)indexPathByAddingIndex:(NSUInteger)index {
    let mut indexes = env.objc.borrow::<NSIndexPathHostObject>(this).indexes.clone();
    indexes.push(index);
    index_path(env, indexes)
}
- (id)indexPathByRemovingLastIndex {
    let mut indexes = env.objc.borrow::<NSIndexPathHostObject>(this).indexes.clone();
    indexes.pop();
    index_path(env, indexes)
}

- (NSUInteger)section {
    section_and_row(env, this).0
}
- (NSUInteger)row {
    section_and_row(env, this).1
}

- (bool)isEqual:(id)other {
    let class = env.objc.get_known_class("NSIndexPath", &mut env.mem);
    let is_path: bool = msg![env; other isKindOfClass:class];
    is_path && indexes(env, this) == indexes(env, other)
}
- (NSUInteger)hash {
    indexes(env, this)
        .iter()
        .fold(17u32, |hash, &index| hash.wrapping_mul(31).wrapping_add(index))
}
- (NSComparisonResult)compare:(id)other {
    match indexes(env, this).cmp(indexes(env, other)) {
        std::cmp::Ordering::Less => NSOrderedAscending,
        std::cmp::Ordering::Equal => NSOrderedSame,
        std::cmp::Ordering::Greater => NSOrderedDescending,
    }
}

- (id)description {
    let list = indexes(env, this)
        .iter()
        .map(|index| index.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    let text = from_rust_string(env, format!("<NSIndexPath {{{}}}>", list));
    autorelease(env, text)
}

@end

};
