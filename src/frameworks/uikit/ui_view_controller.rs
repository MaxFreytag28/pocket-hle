/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `UIViewController`.
//!
//! Resources:
//! - [View Controller Programming Guide for iOS (Legacy)](https://developer.apple.com/library/archive/documentation/WindowsViews/Conceptual/ViewControllerPGforiOSLegacy/BasicViewControllers/BasicViewControllers.html)

use crate::frameworks::core_graphics::CGRect;
use crate::frameworks::foundation::ns_objc_runtime::NSStringFromClass;
use crate::frameworks::foundation::ns_string::{from_rust_string, get_static_str, to_rust_string};
use crate::frameworks::uikit::ui_view::set_view_controller;
use crate::objc::{
    id, msg, msg_class, nil, objc_classes, release, retain, Class, ClassExports, HostObject,
    NSZonePtr,
};
use crate::Environment;

pub mod ui_navigation_controller;
pub mod ui_table_view_controller;

#[derive(Default)]
struct UIViewControllerHostObject {
    /// The root view.
    /// `UIView*`
    view: id,
    /// Nib name to be used at the load
    /// of the root view, may be nil.
    /// `NSString*`
    nib_name: id,
    /// Whether `viewDidLoad` was sent for the view.
    view_did_load_sent: bool,
    /// `NSString*`
    title: id,
    /// `UINavigationItem*`, created when first asked for.
    navigation_item: id,
    /// The controller this one is shown in, e.g. a navigation controller
    /// (weak).
    parent: id,
    /// The controller shown modally over this one (retained), and the one
    /// this one is shown modally over (weak).
    modal: id,
    presenting: id,
    /// Bundle to be used for load
    /// of the nib by name, may be nil.
    /// `NSBundle*`
    bundle: id,
}
impl HostObject for UIViewControllerHostObject {}

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

@implementation UIViewController: UIResponder

+ (id)allocWithZone:(NSZonePtr)_zone {
    let host_object = Box::<UIViewControllerHostObject>::default();
    env.objc.alloc_object(this, host_object, &mut env.mem)
}

// TODO: this should be a designated initializer
- (id)initWithNibName:(id)nib_name // NSString *
               bundle:(id)bundle { // NSBundle *
    retain(env, nib_name);
    retain(env, bundle);

    log_dbg!("[(UIViewController*){:?} initWithNibName:{:?} bundle:{:?}]", this, nib_name, bundle);

    env.objc.borrow_mut::<UIViewControllerHostObject>(this).nib_name = nib_name;
    env.objc.borrow_mut::<UIViewControllerHostObject>(this).bundle = bundle;

    this
}

- (id)initWithCoder:(id)coder {
    let key_ns_string = get_static_str(env, "UIView");
    let view: id = msg![env; coder decodeObjectForKey:key_ns_string];

    () = msg![env; this setView:view];

    let key = get_static_str(env, "UINibName");
    let nib_name: id = msg![env; coder decodeObjectForKey:key];
    retain(env, nib_name);
    let key = get_static_str(env, "UITitle");
    let title: id = msg![env; coder decodeObjectForKey:key];
    let title: id = msg![env; title copy];
    let key = get_static_str(env, "UINavigationItem");
    let navigation_item: id = msg![env; coder decodeObjectForKey:key];
    retain(env, navigation_item);

    let host = env.objc.borrow_mut::<UIViewControllerHostObject>(this);
    host.nib_name = nib_name;
    host.title = title;
    host.navigation_item = navigation_item;
    // The parent (UIParentViewController) isn't decoded: it is still being
    // decoded itself, and decoding it again would never end. The parent sets
    // itself.


    this
}

- (())dealloc {
    let &UIViewControllerHostObject { view, nib_name, bundle, title, navigation_item, modal, .. } = env.objc.borrow(this);
    release(env, title);
    release(env, navigation_item);
    release(env, modal);

    if view != nil {
        set_view_controller(env, view, nil);
    }
    release(env, view);
    release(env, nib_name);
    release(env, bundle);

    env.objc.dealloc_object(this, &mut env.mem);
}

- (())loadView {
    let bundle: id = env.objc.borrow::<UIViewControllerHostObject>(this).bundle;
    let bundle: id = if bundle == nil {
        msg_class![env; NSBundle mainBundle]
    } else {
        bundle
    };

    let nib_name: id = get_nib_name(env, this, bundle);
    if nib_name != nil {
        // If we do have nib name, try to load it!
        log_dbg!(
            "Load {:?} view controller's view by nib, using name {}", this, to_rust_string(env, nib_name)
        );

        let nib: id = msg_class![env; UINib nibWithNibName:nib_name bundle:bundle];
        release(env, nib_name);

        // The NIB's File's Owner will be substituted by `this`,
        // implicitly loading the view as well
        let _: id = msg![env; nib instantiateWithOwner:this options:nil];

        let view = env.objc.borrow::<UIViewControllerHostObject>(this).view;
        // Having nil view at this point probably mean that
        // out nib's parsing is wrong.
        // Also we assume here the case of a "detached nib file"
        // TODO: support "integrated nib file"
        assert!(view != nil);

        return;
    };

    // As a last resort, use plain UIVIew for the root view
    let class: Class = msg![env; this class];
    log!("Unable to load {:?} {} view controller's view by nib, using plain UIView", this, env.objc.get_class_name(class).to_string());
    let view: id = msg_class![env; UIView alloc];
    // Docs are saying that "an empty UIView" is created,
    // but testing reveals that frame matches the screen one
    // (at least on the simulator)
    let screen: id = msg_class![env; UIScreen mainScreen];
    let app_frame: CGRect = msg![env; screen applicationFrame];
    let view: id = msg![env; view initWithFrame:app_frame];
    () = msg![env; this setView:view];
}

- (())setView:(id)new_view { // UIView*
    let host_obj = env.objc.borrow_mut::<UIViewControllerHostObject>(this);
    let old_view = std::mem::replace(&mut host_obj.view, new_view);
    if old_view != nil {
        set_view_controller(env, old_view, nil);
    }
    if new_view != nil {
        set_view_controller(env, new_view, this);
    }
    retain(env, new_view);
    release(env, old_view);
}
// iOS: a view controller's next responder is its view's superview.
- (id)nextResponder {
    let view = env.objc.borrow::<UIViewControllerHostObject>(this).view;
    if view == nil {
        nil
    } else {
        msg![env; view superview]
    }
}

- (id)view {
    let view = env.objc.borrow_mut::<UIViewControllerHostObject>(this).view;
    if view == nil {
        () = msg![env; this loadView];
    }
    let view = env.objc.borrow_mut::<UIViewControllerHostObject>(this).view;
    // Also for a view that came from a nib file with the controller.
    if view != nil && !env.objc.borrow::<UIViewControllerHostObject>(this).view_did_load_sent {
        env.objc.borrow_mut::<UIViewControllerHostObject>(this).view_did_load_sent = true;
        () = msg![env; this viewDidLoad];
    }
    view
}

// Usually overridden by the application
- (())viewDidLoad {
    log_dbg!("[(UIViewController*){:?} viewDidLoad]", this);
}
- (())viewWillAppear:(bool)animated {
    log_dbg!("[(UIViewController*){:?} viewWillAppear:{}]", this, animated);
}
- (())viewDidAppear:(bool)animated {
    log_dbg!("[(UIViewController*){:?} viewDidAppear:{}]", this, animated);
}
- (())viewWillDisappear:(bool)animated {
    log_dbg!("[(UIViewController*){:?} viewWillDisappear:{}]", this, animated);
}
- (())viewDidDisappear:(bool)animated {
    log_dbg!("[(UIViewController*){:?} viewDidDisappear:{}]", this, animated);
}

- (id)title {
    env.objc.borrow::<UIViewControllerHostObject>(this).title
}
- (())setTitle:(id)title { // NSString *
    let title: id = msg![env; title copy];
    let old = std::mem::replace(&mut env.objc.borrow_mut::<UIViewControllerHostObject>(this).title, title);
    release(env, old);
    // The navigation item shows the controller's title.
    let item = env.objc.borrow::<UIViewControllerHostObject>(this).navigation_item;
    if item != nil {
        () = msg![env; item setTitle:title];
    }
}

- (id)navigationItem {
    let item = env.objc.borrow::<UIViewControllerHostObject>(this).navigation_item;
    if item != nil {
        return item;
    }
    let title = env.objc.borrow::<UIViewControllerHostObject>(this).title;
    let item: id = msg_class![env; UINavigationItem alloc];
    let item: id = msg![env; item initWithTitle:title];
    env.objc.borrow_mut::<UIViewControllerHostObject>(this).navigation_item = item;
    item
}

- (id)parentViewController {
    env.objc.borrow::<UIViewControllerHostObject>(this).parent
}
- (())_touchHLE_setParentViewController:(id)parent {
    env.objc.borrow_mut::<UIViewControllerHostObject>(this).parent = parent;
}
- (id)navigationController {
    let class = env.objc.get_known_class("UINavigationController", &mut env.mem);
    let mut controller = env.objc.borrow::<UIViewControllerHostObject>(this).parent;
    while controller != nil {
        let is_navigation: bool = msg![env; controller isKindOfClass:class];
        if is_navigation {
            return controller;
        }
        controller = env.objc.borrow::<UIViewControllerHostObject>(controller).parent;
    }
    nil
}

- (i32)interfaceOrientation {
    // TODO: follow the device's orientation
    1 // UIInterfaceOrientationPortrait
}
- (bool)shouldAutorotateToInterfaceOrientation:(i32)orientation {
    orientation == 1
}

- (id)modalViewController {
    env.objc.borrow::<UIViewControllerHostObject>(this).modal
}
- (())presentModalViewController:(id)controller // UIViewController*
                        animated:(bool)_animated {
    retain(env, controller);
    let old = std::mem::replace(&mut env.objc.borrow_mut::<UIViewControllerHostObject>(this).modal, controller);
    release(env, old);
    env.objc.borrow_mut::<UIViewControllerHostObject>(controller).presenting = this;
    env.objc.borrow_mut::<UIViewControllerHostObject>(controller).parent = this;

    // Shown over everything, filling the application frame.
    let own_view: id = msg![env; this view];
    let window: id = msg![env; own_view window];
    let window: id = if window != nil {
        window
    } else {
        let app: id = msg_class![env; UIApplication sharedApplication];
        msg![env; app keyWindow]
    };
    let screen: id = msg_class![env; UIScreen mainScreen];
    let frame: CGRect = msg![env; screen applicationFrame];
    let view: id = msg![env; controller view];
    () = msg![env; view setFrame:frame];
    () = msg![env; controller viewWillAppear:false];
    () = msg![env; window addSubview:view];
    () = msg![env; controller viewDidAppear:false];
}
- (())setEditing:(bool)editing {
    log!("TODO: [(UIViewController*){:?} setEditing:{}]", this, editing); // TODO
}

- (())dismissModalViewControllerAnimated:(bool)animated {
    let &UIViewControllerHostObject { modal, presenting, .. } = env.objc.borrow(this);
    if modal == nil {
        // Sent to the modal controller itself: the one showing it handles it.
        if presenting != nil {
            () = msg![env; presenting dismissModalViewControllerAnimated:animated];
        }
        return;
    }
    let view: id = msg![env; modal view];
    () = msg![env; modal viewWillDisappear:false];
    () = msg![env; view removeFromSuperview];
    () = msg![env; modal viewDidDisappear:false];
    env.objc.borrow_mut::<UIViewControllerHostObject>(modal).presenting = nil;
    env.objc.borrow_mut::<UIViewControllerHostObject>(this).modal = nil;
    release(env, modal);
}

@end

};

/// A helper function to resolve suitable NIB name for a `view_controller`
/// in the `bundle`. Returns nil if fails.
///
/// Note: It's a responsibility of a caller to release the returned name
/// if not-nil!
fn get_nib_name(env: &mut Environment, view_controller: id, bundle: id) -> id {
    let provider_nib_name: id = env
        .objc
        .borrow::<UIViewControllerHostObject>(view_controller)
        .nib_name;
    if provider_nib_name != nil {
        // TODO: it's not clear how to handle situation when
        // provided nib name do not exist in the bundle.
        // It probably means that our bundle resource loading
        // is faulty, to check
        assert!(check_nib_exists(env, bundle, provider_nib_name));

        retain(env, provider_nib_name);
        return provider_nib_name;
    };

    let class: Class = msg![env; view_controller class];
    let class_name: id = NSStringFromClass(env, class);
    let class_name_str = to_rust_string(env, class_name);

    if let Some(name) = class_name_str.strip_suffix("Controller") {
        let ns_name: id = from_rust_string(env, name.to_string());
        if check_nib_exists(env, bundle, ns_name) {
            release(env, class_name);
            return ns_name;
        }
    }

    if check_nib_exists(env, bundle, class_name) {
        class_name
    } else {
        release(env, class_name);
        nil
    }
}

/// A helper function to check if `nib_name` NIB actually
/// existing in the `bundle`
fn check_nib_exists(env: &mut Environment, bundle: id, nib_name: id) -> bool {
    let type_: id = get_static_str(env, "nib");
    let res: id = msg![env; bundle pathForResource:nib_name ofType:type_];
    res != nil
}
