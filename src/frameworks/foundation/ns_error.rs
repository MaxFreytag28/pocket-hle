/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */

use crate::dyld::{ConstantExports, HostConstant};
use crate::frameworks::foundation::NSInteger;
use super::ns_string::{from_rust_string, get_static_str, to_rust_string};
use crate::objc::{
    autorelease, id, msg, nil, release, retain, ClassExports, HostObject, NSZonePtr,
};
use crate::objc_classes;

/// `NSString*`
pub type NSErrorDomain = id;

pub const NSOSStatusErrorDomain: &str = "NSOSStatusErrorDomain";

struct ErrorHostObject {
    domain: NSErrorDomain,
    code: NSInteger,
    user_info: id,
}
impl HostObject for ErrorHostObject {}

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

// TODO: Return errors in all methods that are supposed to do it.
@implementation NSError: NSObject

+ (id)allocWithZone:(NSZonePtr)_zone {
    let host_object = Box::new(ErrorHostObject {
        domain: nil,
        code: 0,
        user_info: nil
    });
    env.objc.alloc_object(this, host_object, &mut env.mem)
}

- (id)initWithDomain:(NSErrorDomain)domain
                code:(NSInteger)code
            userInfo:(id)user_info {
    retain(env, domain);
    retain(env, user_info);
    let host_obj = env.objc.borrow_mut::<ErrorHostObject>(this);
    host_obj.domain = domain;
    host_obj.code = code;
    host_obj.user_info = user_info;
    this
}

- (())dealloc {
    let &ErrorHostObject{domain, user_info, ..} = env.objc.borrow(this);
    release(env, domain);
    release(env, user_info);

    env.objc.dealloc_object(this, &mut env.mem);
}

- (NSInteger)code {
    env.objc.borrow::<ErrorHostObject>(this).code
}

- (id)domain {
    env.objc.borrow::<ErrorHostObject>(this).domain
}

- (id)userInfo {
    env.objc.borrow::<ErrorHostObject>(this).user_info
}

- (id)localizedDescription {
    let &ErrorHostObject{domain, code, user_info} = env.objc.borrow(this);
    if user_info != nil {
        let key = get_static_str(env, "NSLocalizedDescriptionKey");
        let description: id = msg![env; user_info objectForKey:key];
        if description != nil {
            return description;
        }
    }
    let domain = if domain == nil {
        String::from("(null)")
    } else {
        to_rust_string(env, domain).to_string()
    };
    let description = format!(
        "The operation couldn\u{2019}t be completed. ({} error {}.)",
        domain, code
    );
    let description = from_rust_string(env, description);
    autorelease(env, description)
}

@end

};

pub const CONSTANTS: ConstantExports = &[
    (
        "_NSLocalizedDescriptionKey",
        HostConstant::NSString("NSLocalizedDescriptionKey"),
    ),
    (
        "_NSOSStatusErrorDomain",
        HostConstant::NSString(NSOSStatusErrorDomain),
    ),
];
