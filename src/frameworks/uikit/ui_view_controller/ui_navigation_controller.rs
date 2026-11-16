/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `UINavigationController`.
//!
//! Its view holds the navigation bar at the top and the top view controller's
//! view below it. The bar shows the view controllers' navigation items, and
//! its back button goes back a level.

use crate::frameworks::core_graphics::{CGFloat, CGPoint, CGRect, CGSize};
use crate::frameworks::foundation::ns_string::get_static_str;
use crate::frameworks::foundation::{ns_array, NSUInteger};
use crate::objc::{
    autorelease, id, impl_HostObject_with_superclass, msg, msg_class, msg_send, msg_super, nil,
    objc_classes, release, retain, ClassExports, NSZonePtr, SEL,
};
use crate::Environment;

// TODO: toolbar
// TODO: animations

const BAR_HEIGHT: CGFloat = 44.0;

#[derive(Default)]
struct UINavigationControllerHostObject {
    superclass: super::UIViewControllerHostObject,
    /// something implementing UINavigationControllerDelegate
    delegate: id,
    /// Navigation stack of view controllers, non-retaining
    /// (we explicitly retain/release on push/pop messages)
    navigation_stack: Vec<id>,
    /// `UINavigationBar*`
    navigation_bar: id,
    bar_hidden: bool,
}
impl_HostObject_with_superclass!(UINavigationControllerHostObject);

fn host(env: &mut Environment, this: id) -> &mut UINavigationControllerHostObject {
    env.objc.borrow_mut(this)
}

/// Tell the delegate, if it wants to know, that `controller` will be or was
/// shown.
fn notify_delegate(env: &mut Environment, this: id, controller: id, did: bool) {
    let delegate = host(env, this).delegate;
    if delegate == nil {
        return;
    }
    let name = if did {
        "navigationController:didShowViewController:animated:"
    } else {
        "navigationController:willShowViewController:animated:"
    };
    let sel: SEL = env
        .objc
        .register_host_selector(name.to_string(), &mut env.mem);
    let responds: bool = msg![env; delegate respondsToSelector:sel];
    if responds {
        () = msg_send(env, (delegate, sel, this, controller, false));
    }
}

/// The part of the view below the navigation bar.
fn content_frame(env: &mut Environment, this: id, view: id) -> CGRect {
    let bounds: CGRect = msg![env; view bounds];
    let top = if host(env, this).bar_hidden {
        0.0
    } else {
        BAR_HEIGHT
    };
    CGRect {
        origin: CGPoint { x: 0.0, y: top },
        size: CGSize {
            width: bounds.size.width,
            height: bounds.size.height - top,
        },
    }
}

/// Whether the view has been created yet (without creating it).
fn view_loaded(env: &mut Environment, this: id) -> bool {
    host(env, this).superclass.view != nil
}

/// Show `controller`'s view in place of `old`'s.
fn show(env: &mut Environment, this: id, old: id, controller: id) {
    if !view_loaded(env, this) {
        return;
    }
    let view: id = msg![env; this view];
    notify_delegate(env, this, controller, false);
    if old != nil {
        let old_view: id = msg![env; old view];
        () = msg![env; old viewWillDisappear:false];
        () = msg![env; old_view removeFromSuperview];
        () = msg![env; old viewDidDisappear:false];
    }
    if controller != nil {
        let controller_view: id = msg![env; controller view];
        let frame = content_frame(env, this, view);
        () = msg![env; controller_view setFrame:frame];
        () = msg![env; controller viewWillAppear:false];
        let bar = host(env, this).navigation_bar;
        if bar != nil {
            () = msg![env; view insertSubview:controller_view belowSubview:bar];
        } else {
            () = msg![env; view addSubview:controller_view];
        }
        () = msg![env; controller viewDidAppear:false];
    }
    notify_delegate(env, this, controller, true);
}

/// Make the bar show the navigation items of the controllers on the stack.
fn update_bar_items(env: &mut Environment, this: id) {
    let bar = host(env, this).navigation_bar;
    if bar == nil {
        return;
    }
    let stack = host(env, this).navigation_stack.clone();
    let items: Vec<id> = stack
        .into_iter()
        .map(|controller| {
            let item: id = msg![env; controller navigationItem];
            retain(env, item)
        })
        .collect();
    let items = ns_array::from_vec(env, items);
    () = msg![env; bar setItems:items];
    release(env, items);
}

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

@implementation UINavigationController: UIViewController

+ (id)allocWithZone:(NSZonePtr)_zone {
    let host_object = Box::<UINavigationControllerHostObject>::default();
    env.objc.alloc_object(this, host_object, &mut env.mem)
}

- (id)initWithRootViewController:(id)root_vc { // UIViewController *
    () = msg![env; this pushViewController:root_vc animated:false];
    this
}

- (id)initWithCoder:(id)coder {
    let this: id = msg_super![env; this initWithCoder:coder];

    let key = get_static_str(env, "UINavigationBar");
    let bar: id = msg![env; coder decodeObjectForKey:key];
    retain(env, bar);
    host(env, this).navigation_bar = bar;

    let key = get_static_str(env, "UIViewControllers");
    let controllers: id = msg![env; coder decodeObjectForKey:key];
    if controllers != nil {
        let count: NSUInteger = msg![env; controllers count];
        for i in 0..count {
            let controller: id = msg![env; controllers objectAtIndex:i];
            retain(env, controller);
            () = msg![env; controller _touchHLE_setParentViewController:this];
            host(env, this).navigation_stack.push(controller);
        }
    }
    this
}

- (())dealloc {
    let host = host(env, this);
    let stack = std::mem::take(&mut host.navigation_stack);
    let bar = host.navigation_bar;
    for controller in stack {
        release(env, controller);
    }
    release(env, bar);
    msg_super![env; this dealloc]
}

- (())loadView {
    let screen: id = msg_class![env; UIScreen mainScreen];
    let frame: CGRect = msg![env; screen applicationFrame];
    let view: id = msg_class![env; UIView alloc];
    let view: id = msg![env; view initWithFrame:frame];
    () = msg![env; this setView:view];
    release(env, view);

    let bar_frame = CGRect {
        origin: CGPoint { x: 0.0, y: 0.0 },
        size: CGSize { width: frame.size.width, height: BAR_HEIGHT },
    };
    if host(env, this).navigation_bar == nil {
        let bar: id = msg_class![env; UINavigationBar alloc];
        let bar: id = msg![env; bar initWithFrame:bar_frame];
        host(env, this).navigation_bar = bar;
    }
    let bar = host(env, this).navigation_bar;
    () = msg![env; bar setFrame:bar_frame];
    () = msg![env; bar setDelegate:this];
    let hidden = host(env, this).bar_hidden;
    () = msg![env; bar setHidden:hidden];
    () = msg![env; view addSubview:bar];
    update_bar_items(env, this);

    let top = host(env, this).navigation_stack.last().copied().unwrap_or(nil);
    show(env, this, nil, top);
}

// weak/non-retaining
- (())setDelegate:(id)delegate { // something implementing UINavigationControllerDelegate
    host(env, this).delegate = delegate;
}
- (id)delegate {
    host(env, this).delegate
}

- (id)navigationBar {
    let _: id = msg![env; this view];
    host(env, this).navigation_bar
}
- (bool)isNavigationBarHidden {
    host(env, this).bar_hidden
}
- (())setNavigationBarHidden:(bool)hidden {
    host(env, this).bar_hidden = hidden;
    if !view_loaded(env, this) {
        return;
    }
    let bar = host(env, this).navigation_bar;
    () = msg![env; bar setHidden:hidden];
    let top = host(env, this).navigation_stack.last().copied().unwrap_or(nil);
    if top != nil {
        let view: id = msg![env; this view];
        let frame = content_frame(env, this, view);
        let top_view: id = msg![env; top view];
        () = msg![env; top_view setFrame:frame];
    }
}
- (())setNavigationBarHidden:(bool)hidden animated:(bool)_animated {
    () = msg![env; this setNavigationBarHidden:hidden];
}
- (())setToolbarHidden:(bool)_hidden {}
- (())setToolbarHidden:(bool)_hidden animated:(bool)_animated {}

- (())pushViewController:(id)view_controller // UIViewController *
                animated:(bool)_animated {
    let old = host(env, this).navigation_stack.last().copied().unwrap_or(nil);
    assert!(!host(env, this).navigation_stack.contains(&view_controller));
    retain(env, view_controller);
    host(env, this).navigation_stack.push(view_controller);
    () = msg![env; view_controller _touchHLE_setParentViewController:this];
    update_bar_items(env, this);
    show(env, this, old, view_controller);
}

- (id)popViewControllerAnimated:(bool)_animated {
    if host(env, this).navigation_stack.len() <= 1 {
        return nil;
    }
    let old = host(env, this).navigation_stack.pop().unwrap();
    let new = host(env, this).navigation_stack.last().copied().unwrap();
    update_bar_items(env, this);
    show(env, this, old, new);
    autorelease(env, old)
}
- (id)popToViewController:(id)controller animated:(bool)_animated {
    let stack = &host(env, this).navigation_stack;
    let Some(index) = stack.iter().position(|&c| c == controller) else {
        return nil;
    };
    let popped: Vec<id> = host(env, this).navigation_stack.split_off(index + 1);
    if popped.is_empty() {
        return nil;
    }
    update_bar_items(env, this);
    show(env, this, *popped.last().unwrap(), controller);
    let array = ns_array::from_vec(env, popped);
    autorelease(env, array)
}
- (id)popToRootViewControllerAnimated:(bool)animated {
    let root = host(env, this).navigation_stack.first().copied().unwrap_or(nil);
    if root == nil {
        return nil;
    }
    msg![env; this popToViewController:root animated:animated]
}

- (id)topViewController {
    host(env, this).navigation_stack.last().copied().unwrap_or(nil)
}
- (id)visibleViewController {
    let top: id = msg![env; this topViewController];
    let modal: id = if top != nil { msg![env; top modalViewController] } else { nil };
    if modal != nil { modal } else { top }
}

- (id)viewControllers {
    let vcs = host(env, this).navigation_stack.to_vec();
    for vc in &vcs {
        retain(env, *vc);
    }
    let res = ns_array::from_vec(env, vcs);
    autorelease(env, res)
}
- (())setViewControllers:(id)controllers { // NSArray *
    msg![env; this setViewControllers:controllers animated:false]
}
- (())setViewControllers:(id)controllers // NSArray *
                animated:(bool)_animated {
    let old_top = host(env, this).navigation_stack.last().copied().unwrap_or(nil);
    let count: NSUInteger = msg![env; controllers count];
    let mut new_stack = Vec::new();
    for i in 0..count {
        let controller: id = msg![env; controllers objectAtIndex:i];
        retain(env, controller);
        () = msg![env; controller _touchHLE_setParentViewController:this];
        new_stack.push(controller);
    }
    let new_top = new_stack.last().copied().unwrap_or(nil);
    let old_stack = std::mem::replace(&mut host(env, this).navigation_stack, new_stack);
    update_bar_items(env, this);
    if old_top != new_top {
        show(env, this, old_top, new_top);
    }
    for controller in old_stack {
        release(env, controller);
    }
}

// The navigation bar's back button.
- (bool)navigationBar:(id)_bar shouldPopItem:(id)_item {
    let _: id = msg![env; this popViewControllerAnimated:true];
    // The bar is updated by the pop.
    false
}

@end

};
