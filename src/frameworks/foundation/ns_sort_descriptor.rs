/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `NSSortDescriptor`.

use super::NSComparisonResult;
use crate::objc::{
    id, msg, msg_send, nil, objc_classes, release, retain, ClassExports, HostObject, NSZonePtr,
    SEL,
};
use crate::Environment;

struct SortDescriptorHostObject {
    /// `NSString*`
    key: id,
    ascending: bool,
    /// `compare:` if not specified.
    selector: Option<SEL>,
}
impl HostObject for SortDescriptorHostObject {}

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

@implementation NSSortDescriptor: NSObject

+ (id)allocWithZone:(NSZonePtr)_zone {
    let host_object = Box::new(SortDescriptorHostObject {
        key: nil,
        ascending: true,
        selector: None,
    });
    env.objc.alloc_object(this, host_object, &mut env.mem)
}

- (id)initWithKey:(id)key // NSString*
        ascending:(bool)ascending {
    let key: id = msg![env; key copy];
    let host_object = env.objc.borrow_mut::<SortDescriptorHostObject>(this);
    host_object.key = key;
    host_object.ascending = ascending;
    this
}

- (id)initWithKey:(id)key // NSString*
        ascending:(bool)ascending
         selector:(SEL)selector {
    let key: id = msg![env; key copy];
    let host_object = env.objc.borrow_mut::<SortDescriptorHostObject>(this);
    host_object.key = key;
    host_object.ascending = ascending;
    host_object.selector = Some(selector);
    this
}

- (id)key {
    env.objc.borrow::<SortDescriptorHostObject>(this).key
}
- (bool)ascending {
    env.objc.borrow::<SortDescriptorHostObject>(this).ascending
}

- (id)retainedCopy {
    retain(env, this)
}

- (())dealloc {
    let key = env.objc.borrow::<SortDescriptorHostObject>(this).key;
    release(env, key);
    env.objc.dealloc_object(this, &mut env.mem)
}

@end

};

/// Compare two objects using a list of `NSSortDescriptor*`: the first
/// descriptor that does not consider them equal decides.
pub fn compare_using_descriptors(
    env: &mut Environment,
    descriptors: &[id],
    a: id,
    b: id,
) -> NSComparisonResult {
    for &descriptor in descriptors {
        let (key, ascending, selector) = {
            let host_object = env.objc.borrow::<SortDescriptorHostObject>(descriptor);
            (host_object.key, host_object.ascending, host_object.selector)
        };
        let a_value: id = msg![env; a valueForKey:key];
        let b_value: id = msg![env; b valueForKey:key];
        let res: NSComparisonResult = if a_value == nil || b_value == nil {
            // nil sorts first.
            (b_value == nil) as NSComparisonResult - (a_value == nil) as NSComparisonResult
        } else {
            let selector =
                selector.unwrap_or_else(|| env.objc.lookup_selector("compare:").unwrap());
            msg_send(env, (a_value, selector, b_value))
        };
        if res != 0 {
            return if ascending { res } else { -res };
        }
    }
    0
}
