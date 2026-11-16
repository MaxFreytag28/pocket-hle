/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `UIPickerView`.
//!
//! Drawn simply: each component shows its selected row's title between
//! buttons for the previous and next rows. Rows given as views aren't shown.

use crate::frameworks::core_graphics::{CGFloat, CGPoint, CGRect, CGSize};
use crate::frameworks::foundation::ns_string::get_static_str;
use crate::frameworks::foundation::NSInteger;
use crate::frameworks::uikit::ui_font::UITextAlignmentCenter;
use crate::frameworks::uikit::ui_view::ui_control::ui_button::UIButtonTypeCustom;
use crate::frameworks::uikit::ui_view::ui_control::{
    UIControlEventTouchUpInside, UIControlStateNormal,
};
use crate::objc::{
    id, impl_HostObject_with_superclass, msg, msg_class, msg_send, msg_super, nil, objc_classes,
    release, retain, ClassExports, NSZonePtr, SEL,
};
use crate::Environment;

#[derive(Default)]
struct UIPickerViewHostObject {
    superclass: super::UIViewHostObject,
    /// Weak.
    data_source: id,
    delegate: id,
    /// The selected row of each component.
    selected: Vec<NSInteger>,
    /// Per component: title label, previous and next buttons (retained,
    /// subviews).
    parts: Vec<(id, id, id)>,
}
impl_HostObject_with_superclass!(UIPickerViewHostObject);

fn host(env: &mut Environment, this: id) -> &mut UIPickerViewHostObject {
    env.objc.borrow_mut(this)
}

fn responds(env: &mut Environment, object: id, name: &str) -> Option<SEL> {
    if object == nil {
        return None;
    }
    let sel = env
        .objc
        .register_host_selector(name.to_string(), &mut env.mem);
    let responds: bool = msg![env; object respondsToSelector:sel];
    responds.then_some(sel)
}

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

@implementation UIPickerView: UIView

+ (id)allocWithZone:(NSZonePtr)_zone {
    let host_object = Box::<UIPickerViewHostObject>::default();
    env.objc.alloc_object(this, host_object, &mut env.mem)
}

- (())dealloc {
    let parts = std::mem::take(&mut host(env, this).parts);
    for (label, previous, next) in parts {
        release(env, label);
        release(env, previous);
        release(env, next);
    }
    msg_super![env; this dealloc]
}

- (())setShowsSelectionIndicator:(bool)_shows {}
- (id)delegate {
    host(env, this).delegate
}
- (())setDelegate:(id)delegate {
    host(env, this).delegate = delegate;
    () = msg![env; this reloadAllComponents];
}
- (id)dataSource {
    host(env, this).data_source
}
- (())setDataSource:(id)data_source {
    host(env, this).data_source = data_source;
    () = msg![env; this reloadAllComponents];
}

- (NSInteger)numberOfComponents {
    host(env, this).selected.len() as NSInteger
}
- (NSInteger)selectedRowInComponent:(NSInteger)component {
    host(env, this).selected.get(component as usize).copied().unwrap_or(-1)
}
- (())selectRow:(NSInteger)row inComponent:(NSInteger)component animated:(bool)_animated {
    let selected = &mut host(env, this).selected;
    if let Some(slot) = selected.get_mut(component as usize) {
        *slot = row;
    }
    update_titles(env, this);
}
- (())reloadAllComponents {
    rebuild(env, this);
}
- (())reloadComponent:(NSInteger)_component {
    update_titles(env, this);
}

- (())setFrame:(CGRect)frame {
    () = msg_super![env; this setFrame:frame];
    rebuild(env, this);
}

- (())_touchHLE_stepTapped:(id)sender {
    let parts = host(env, this).parts.clone();
    for (component, (_, previous, next)) in parts.into_iter().enumerate() {
        let step = if sender == previous { -1 } else if sender == next { 1 } else { continue };
        let rows = row_count(env, this, component as NSInteger);
        let current = host(env, this).selected[component];
        let new = (current + step).clamp(0, (rows - 1).max(0));
        if new == current {
            return;
        }
        host(env, this).selected[component] = new;
        update_titles(env, this);
        let delegate = host(env, this).delegate;
        if let Some(sel) = responds(env, delegate, "pickerView:didSelectRow:inComponent:") {
            () = msg_send(env, (delegate, sel, this, new, component as NSInteger));
        }
        return;
    }
}

@end

};

fn row_count(env: &mut Environment, this: id, component: NSInteger) -> NSInteger {
    let data_source = host(env, this).data_source;
    responds(env, data_source, "pickerView:numberOfRowsInComponent:")
        .map_or(0, |sel| msg_send(env, (data_source, sel, this, component)))
}

/// Make the parts for each component.
fn rebuild(env: &mut Environment, this: id) {
    let old = std::mem::take(&mut host(env, this).parts);
    for (label, previous, next) in old {
        for view in [label, previous, next] {
            () = msg![env; view removeFromSuperview];
            release(env, view);
        }
    }
    let data_source = host(env, this).data_source;
    let components: NSInteger = responds(env, data_source, "numberOfComponentsInPickerView:")
        .map_or(0, |sel| msg_send(env, (data_source, sel, this)));
    host(env, this)
        .selected
        .resize(components.max(0) as usize, 0);

    let white: id = msg_class![env; UIColor whiteColor];
    () = msg![env; this setBackgroundColor:white];
    let bounds: CGRect = msg![env; this bounds];
    let width = bounds.size.width / (components.max(1) as CGFloat);
    let height = bounds.size.height;
    let action = env.objc.lookup_selector("_touchHLE_stepTapped:").unwrap();
    let rect = |x: CGFloat, y: CGFloat, w: CGFloat, h: CGFloat| CGRect {
        origin: CGPoint { x, y },
        size: CGSize {
            width: w,
            height: h,
        },
    };
    for component in 0..components.max(0) {
        let x = component as CGFloat * width;
        let label: id = msg_class![env; UILabel alloc];
        let label: id = msg![env; label initWithFrame:(rect(x, height / 3.0, width, height / 3.0))];
        () = msg![env; label setTextAlignment:UITextAlignmentCenter];
        let mut buttons = [nil; 2];
        for (i, text) in ["^", "v"].into_iter().enumerate() {
            let button: id = msg_class![env; UIButton buttonWithType:UIButtonTypeCustom];
            retain(env, button);
            let text = get_static_str(env, text);
            () = msg![env; button setTitle:text forState:UIControlStateNormal];
            let gray: id = msg_class![env; UIColor grayColor];
            () = msg![env; button setTitleColor:gray forState:UIControlStateNormal];
            let frame = rect(
                x,
                if i == 0 { 0.0 } else { height * 2.0 / 3.0 },
                width,
                height / 3.0,
            );
            () = msg![env; button setFrame:frame];
            () = msg![env; button addTarget:this action:action forControlEvents:UIControlEventTouchUpInside];
            () = msg![env; this addSubview:button];
            buttons[i] = button;
        }
        () = msg![env; this addSubview:label];
        host(env, this).parts.push((label, buttons[0], buttons[1]));
    }
    update_titles(env, this);
}

fn update_titles(env: &mut Environment, this: id) {
    let delegate = host(env, this).delegate;
    let title_sel = responds(env, delegate, "pickerView:titleForRow:forComponent:");
    let parts = host(env, this).parts.clone();
    for (component, (label, _, _)) in parts.into_iter().enumerate() {
        let row = host(env, this)
            .selected
            .get(component)
            .copied()
            .unwrap_or(0);
        let title: id = title_sel.map_or(nil, |sel| {
            msg_send(env, (delegate, sel, this, row, component as NSInteger))
        });
        () = msg![env; label setText:title];
    }
}
