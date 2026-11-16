/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `UISegmentedControl`.
//!
//! Drawn simply, as a row of buttons; the selected one is highlighted.
//! Segments from nib files aren't decoded yet.

use super::{send_actions, UIControlEventTouchUpInside, UIControlEventValueChanged};
use crate::frameworks::core_graphics::{CGFloat, CGPoint, CGRect, CGSize};
use crate::frameworks::foundation::{NSInteger, NSUInteger};
use crate::frameworks::uikit::ui_view::ui_control::ui_button::UIButtonTypeCustom;
use crate::frameworks::uikit::ui_view::ui_control::UIControlStateNormal;
use crate::objc::{
    id, impl_HostObject_with_superclass, msg, msg_class, msg_super, nil, objc_classes, release,
    retain, ClassExports, NSZonePtr,
};
use crate::Environment;

const UISegmentedControlNoSegment: NSInteger = -1;

struct UISegmentedControlHostObject {
    superclass: super::UIControlHostObject,
    /// `NSString*` or `UIImage*` per segment (retained).
    contents: Vec<id>,
    /// A button per segment (retained, subviews).
    buttons: Vec<id>,
    selected: NSInteger,
    momentary: bool,
    style: NSInteger,
    /// `UIColor*`
    tint_color: id,
}
impl_HostObject_with_superclass!(UISegmentedControlHostObject);
impl Default for UISegmentedControlHostObject {
    fn default() -> Self {
        UISegmentedControlHostObject {
            superclass: Default::default(),
            contents: Vec::new(),
            buttons: Vec::new(),
            selected: UISegmentedControlNoSegment,
            momentary: false,
            style: 0,
            tint_color: nil,
        }
    }
}

fn host(env: &mut Environment, this: id) -> &mut UISegmentedControlHostObject {
    env.objc.borrow_mut(this)
}

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

@implementation UISegmentedControl: UIControl

+ (id)allocWithZone:(NSZonePtr)_zone {
    let host_object = Box::<UISegmentedControlHostObject>::default();
    env.objc.alloc_object(this, host_object, &mut env.mem)
}

- (id)initWithItems:(id)items { // NSArray*
    let count: NSUInteger = if items == nil { 0 } else { msg![env; items count] };
    let frame = CGRect {
        origin: CGPoint { x: 0.0, y: 0.0 },
        size: CGSize { width: (count.max(1) as CGFloat) * 60.0, height: 30.0 },
    };
    let this: id = msg![env; this initWithFrame:frame];
    for i in 0..count {
        let content: id = msg![env; items objectAtIndex:i];
        retain(env, content);
        host(env, this).contents.push(content);
    }
    rebuild(env, this);
    this
}

// NSCoding implementation
- (id)initWithCoder:(id)coder {
    log!("TODO: segments of a UISegmentedControl in a nib file aren't decoded");
    msg_super![env; this initWithCoder:coder]
}

- (())dealloc {
    let host = host(env, this);
    let contents = std::mem::take(&mut host.contents);
    let buttons = std::mem::take(&mut host.buttons);
    let tint_color = host.tint_color;
    release(env, tint_color);
    for object in contents.into_iter().chain(buttons) {
        release(env, object);
    }
    msg_super![env; this dealloc]
}

- (NSUInteger)numberOfSegments {
    host(env, this).contents.len() as NSUInteger
}
- (NSInteger)selectedSegmentIndex {
    host(env, this).selected
}
- (())setSelectedSegmentIndex:(NSInteger)index {
    host(env, this).selected = index;
    show_selection(env, this);
}
- (bool)isMomentary {
    host(env, this).momentary
}
- (())setMomentary:(bool)momentary {
    host(env, this).momentary = momentary;
}
- (NSInteger)segmentedControlStyle {
    host(env, this).style
}
- (())setSegmentedControlStyle:(NSInteger)style {
    host(env, this).style = style;
}
- (id)tintColor {
    host(env, this).tint_color
}
- (())setTintColor:(id)color { // UIColor*
    retain(env, color);
    let old = std::mem::replace(&mut host(env, this).tint_color, color);
    release(env, old);
    show_selection(env, this);
}
- (id)titleForSegmentAtIndex:(NSUInteger)index {
    let class = env.objc.get_known_class("NSString", &mut env.mem);
    let content = host(env, this).contents.get(index as usize).copied().unwrap_or(nil);
    let is_string: bool = content != nil && msg![env; content isKindOfClass:class];
    if is_string { content } else { nil }
}
- (())setTitle:(id)title forSegmentAtIndex:(NSUInteger)index {
    set_content(env, this, index, title);
}
- (())setImage:(id)image forSegmentAtIndex:(NSUInteger)index {
    set_content(env, this, index, image);
}
- (())setEnabled:(bool)_enabled forSegmentAtIndex:(NSUInteger)_index {}
- (())setWidth:(CGFloat)_width forSegmentAtIndex:(NSUInteger)_index {}

- (())setFrame:(CGRect)frame {
    () = msg_super![env; this setFrame:frame];
    lay_out(env, this);
}

- (())_touchHLE_segmentTapped:(id)sender {
    let Some(index) = host(env, this).buttons.iter().position(|&b| b == sender) else {
        return;
    };
    host(env, this).selected = index as NSInteger;
    show_selection(env, this);
    send_actions(env, this, nil, UIControlEventValueChanged);
    // A momentary control doesn't stay selected.
    if host(env, this).momentary {
        host(env, this).selected = UISegmentedControlNoSegment;
        show_selection(env, this);
    }
}

@end

// Undocumented class used by UISegmentedControl
@implementation UISegment: UIControl

- (id)initWithFrame:(CGRect)frame {
    log!("[(UISegment*){:?} initWithFrame:{:?}] Attempted to initialize undocumented class from outside of the NIB.", this, frame);
    unreachable!()
}

// NSCoding implementation
- (id)initWithCoder:(id)coder {
    log!("[(UISegment*){:?} initWithCoder:{:?}] TODO: Implement UISegment. The control won't be rendered.", this, coder);
    msg_super![env; this initWithCoder:coder]
}

@end

};

fn set_content(env: &mut Environment, this: id, index: NSUInteger, content: id) {
    let index = index as usize;
    retain(env, content);
    let contents = &mut host(env, this).contents;
    if index < contents.len() {
        let old = std::mem::replace(&mut contents[index], content);
        release(env, old);
    } else {
        contents.push(content);
    }
    rebuild(env, this);
}

/// Make a button for each segment.
fn rebuild(env: &mut Environment, this: id) {
    let old = std::mem::take(&mut host(env, this).buttons);
    for button in old {
        () = msg![env; button removeFromSuperview];
        release(env, button);
    }
    let contents = host(env, this).contents.clone();
    let string_class = env.objc.get_known_class("NSString", &mut env.mem);
    let white: id = msg_class![env; UIColor whiteColor];
    let font: id = msg_class![env; UIFont boldSystemFontOfSize:(13.0 as CGFloat)];
    let action = env
        .objc
        .lookup_selector("_touchHLE_segmentTapped:")
        .unwrap();
    for content in contents {
        let button: id = msg_class![env; UIButton buttonWithType:UIButtonTypeCustom];
        retain(env, button);
        let is_string: bool = msg![env; content isKindOfClass:string_class];
        if is_string {
            () = msg![env; button setTitle:content forState:UIControlStateNormal];
            () = msg![env; button setTitleColor:white forState:UIControlStateNormal];
            () = msg![env; button setFont:font];
        } else {
            () = msg![env; button setImage:content forState:UIControlStateNormal];
        }
        () = msg![env; button addTarget:this action:action forControlEvents:UIControlEventTouchUpInside];
        () = msg![env; this addSubview:button];
        host(env, this).buttons.push(button);
    }
    lay_out(env, this);
    show_selection(env, this);
}

fn lay_out(env: &mut Environment, this: id) {
    let buttons = host(env, this).buttons.clone();
    if buttons.is_empty() {
        return;
    }
    let bounds: CGRect = msg![env; this bounds];
    let width = bounds.size.width / buttons.len() as CGFloat;
    for (i, button) in buttons.into_iter().enumerate() {
        let frame = CGRect {
            origin: CGPoint {
                x: (i as CGFloat * width).round(),
                y: 0.0,
            },
            size: CGSize {
                width: width.round() - 1.0,
                height: bounds.size.height,
            },
        };
        () = msg![env; button setFrame:frame];
        // Buttons don't place their image themselves: centre it.
        let image: id = msg![env; button currentImage];
        if image != nil {
            let size: CGSize = msg![env; image size];
            let image_view: id = msg![env; button imageView];
            let image_frame = CGRect {
                origin: CGPoint {
                    x: ((frame.size.width - size.width) / 2.0).round(),
                    y: ((frame.size.height - size.height) / 2.0).round(),
                },
                size,
            };
            () = msg![env; image_view setFrame:image_frame];
        }
    }
}

fn show_selection(env: &mut Environment, this: id) {
    let selected = host(env, this).selected;
    let buttons = host(env, this).buttons.clone();
    for (i, button) in buttons.into_iter().enumerate() {
        let color: id = if i as NSInteger == selected {
            msg_class![env; UIColor colorWithRed:(0.2 as CGFloat) green:(0.4 as CGFloat) blue:(0.8 as CGFloat) alpha:(1.0 as CGFloat)]
        } else {
            msg_class![env; UIColor colorWithRed:(0.45 as CGFloat) green:(0.5 as CGFloat) blue:(0.58 as CGFloat) alpha:(1.0 as CGFloat)]
        };
        () = msg![env; button setBackgroundColor:color];
    }
}
