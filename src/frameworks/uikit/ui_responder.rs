/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `UIResponder`.

use crate::objc::{id, msg, nil, objc_classes, ClassExports, SEL};
use crate::Environment;

/// Find the object an action with no target (or one aimed at a nib's
/// "first responder" placeholder) goes to: the first object in the responder
/// chain starting at `start` that implements it, as iOS does.
pub fn find_action_target(env: &mut Environment, start: id, action: SEL) -> id {
    let first_responder = env.framework_state.uikit.ui_responder.first_responder;
    for begin in [first_responder, start] {
        let mut responder = begin;
        // Guard against loops in a broken chain.
        for _ in 0..64 {
            if responder == nil {
                break;
            }
            let responds: bool = msg![env; responder respondsToSelector:action];
            if responds {
                return responder;
            }
            responder = msg![env; responder nextResponder];
        }
    }
    nil
}

/// Whether `target` is unset or a nib's placeholder for the first responder.
pub fn is_first_responder_target(env: &mut Environment, target: id) -> bool {
    if target == nil {
        return true;
    }
    let class: crate::objc::Class = msg![env; target class];
    env.objc.get_class_name(class) == "UIProxyObject"
}

#[derive(Default)]
pub struct State {
    pub(crate) first_responder: id,
}

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

@implementation UIResponder: NSObject

// TODO: real responder implementation etc

// The default implementation of these methods forward the message
// up the responder chain

- (())touchesBegan:(id)touches // NSSet* of UITouch*
         withEvent:(id)event { // UIEvent*
    log_dbg!(
        "[{:?} touchesBegan:{:?} withEvent:{:?}] (probably unhandled)",
        this,
        touches,
        event,
    );
    let next_responder: id = msg![env; this nextResponder];
    if next_responder != nil {
        () = msg![env; next_responder touchesBegan:touches withEvent:event];
    }
}

- (())touchesMoved:(id)touches // NSSet* of UITouch*
         withEvent:(id)event { // UIEvent*
    log_dbg!(
        "[{:?} touchesMoved:{:?} withEvent:{:?}] (probably unhandled)",
        this,
        touches,
        event,
    );
    let next_responder: id = msg![env; this nextResponder];
    if next_responder != nil {
        () = msg![env; next_responder touchesMoved:touches withEvent:event];
    }
}

- (())touchesEnded:(id)touches // NSSet* of UITouch*
         withEvent:(id)event { // UIEvent*
    log_dbg!(
        "[{:?} touchesEnded:{:?} withEvent:{:?}] (probably unhandled)",
        this,
        touches,
        event,
    );
    let next_responder: id = msg![env; this nextResponder];
    if next_responder != nil {
        () = msg![env; next_responder touchesEnded:touches withEvent:event];
    }
}

- (id)nextResponder {
    nil
}

- (bool)isFirstResponder {
    false
}
- (bool)canBecomeFirstResponder {
    false
}
- (bool)becomeFirstResponder {
    // TODO
    false
}
- (bool)canResignFirstResponder {
    true
}
- (bool)resignFirstResponder {
    // TODO
    true
}

@end

};
