/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `UINavigationBar`, `UINavigationItem`, `UIBarButtonItem` and `UIToolbar`.
//!
//! The bar is drawn simply: a coloured strip with the top item's title in
//! the middle and its bar button items as plain buttons on the left and
//! right (a back button on the left when there is an item to go back to).

use crate::abi::GuestRet;
use crate::frameworks::core_graphics::{CGFloat, CGPoint, CGRect, CGSize};
use crate::frameworks::foundation::ns_string::{from_rust_string, get_static_str, to_rust_string};
use crate::frameworks::foundation::{NSInteger, NSUInteger};
use crate::frameworks::uikit::ui_font::UITextAlignmentCenter;
use crate::frameworks::uikit::ui_view::ui_control::ui_button::UIButtonTypeCustom;
use crate::frameworks::uikit::ui_view::ui_control::{
    UIControlEventTouchUpInside, UIControlStateNormal,
};
use crate::objc::{
    autorelease, id, impl_HostObject_with_superclass, msg, msg_class, msg_send, msg_super, nil,
    objc_classes, release, retain, ClassExports, HostObject, NSZonePtr, SEL,
};
use crate::Environment;

type UIBarStyle = NSInteger;
const UIBarStyleDefault: UIBarStyle = 0;

type UIBarButtonSystemItem = NSInteger;

/// The height of a navigation bar in portrait orientation.
const BAR_HEIGHT: CGFloat = 44.0;

fn null_selector() -> SEL {
    <SEL as GuestRet>::from_regs(&[0])
}

struct UIBarButtonItemHostObject {
    /// `NSString*`
    title: id,
    /// `UIView*`
    custom_view: id,
    /// Weak.
    target: id,
    action: Option<SEL>,
    style: NSInteger,
    enabled: bool,
    width: CGFloat,
    tag: NSInteger,
    /// For the standard items: which one.
    system_item: Option<UIBarButtonSystemItem>,
}
impl HostObject for UIBarButtonItemHostObject {}
impl Default for UIBarButtonItemHostObject {
    fn default() -> Self {
        UIBarButtonItemHostObject {
            title: nil,
            custom_view: nil,
            target: nil,
            action: None,
            style: 0,
            enabled: true,
            width: 0.0,
            tag: 0,
            system_item: None,
        }
    }
}

#[derive(Default)]
struct UINavigationItemHostObject {
    /// `NSString*`
    title: id,
    /// `UIBarButtonItem*`
    left: id,
    /// `UIBarButtonItem*`
    right: id,
    /// `UIBarButtonItem*`
    back: id,
    /// `UIView*`
    title_view: id,
    hides_back_button: bool,
    /// The bar showing this item, if any (weak), to update when it changes.
    bar: id,
}
impl HostObject for UINavigationItemHostObject {}

#[derive(Default)]
struct UINavigationBarHostObject {
    superclass: super::UIViewHostObject,
    /// `UINavigationItem*`s, retained.
    items: Vec<id>,
    bar_style: UIBarStyle,
    /// `UIColor*`
    tint_color: id,
    /// Weak.
    delegate: id,
    /// `UILabel*` for the title (a subview).
    title_label: id,
    /// `UIButton*`s for the left and right items (subviews).
    left_button: id,
    right_button: id,
    /// Custom views of the bar button items currently shown, if any.
    shown_custom_views: Vec<id>,
}
impl_HostObject_with_superclass!(UINavigationBarHostObject);

#[derive(Default)]
struct UIToolbarHostObject {
    superclass: super::UIViewHostObject,
    /// `UIBarButtonItem*`s, retained.
    items: Vec<id>,
    bar_style: NSInteger,
    /// `UIColor*`
    tint_color: id,
    /// The buttons shown for the items (retained, subviews), with their item.
    buttons: Vec<(id, id)>,
}
impl_HostObject_with_superclass!(UIToolbarHostObject);

const UIBarButtonSystemItemFlexibleSpace: UIBarButtonSystemItem = 5;
const UIBarButtonSystemItemFixedSpace: UIBarButtonSystemItem = 6;

/// The text shown for one of the standard bar button items.
fn system_item_title(item: UIBarButtonSystemItem) -> &'static str {
    match item {
        0 => "Done",
        1 => "Cancel",
        2 => "Edit",
        3 => "Save",
        4 => "+",
        7 => "Reply",
        8 => "Action",
        9 => "Organize",
        10 => "Bookmarks",
        11 => "Search",
        12 => "Refresh",
        13 => "Stop",
        14 => "Camera",
        15 => "Trash",
        16 => "Play",
        17 => "Pause",
        18 => "Rewind",
        19 => "Forward",
        20 => "Undo",
        21 => "Redo",
        _ => "",
    }
}

fn set_retained(env: &mut Environment, slot: impl FnOnce(&mut Environment) -> &mut id, new: id) {
    retain(env, new);
    let old = std::mem::replace(slot(env), new);
    release(env, old);
}

/// Lay out the bar again after one of its items changed.
fn update_bar_of_item(env: &mut Environment, item: id) {
    let bar = env.objc.borrow::<UINavigationItemHostObject>(item).bar;
    if bar != nil {
        () = msg![env; bar layoutSubviews];
    }
}

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

@implementation UIBarButtonItem: NSObject

+ (id)allocWithZone:(NSZonePtr)_zone {
    let host_object = Box::<UIBarButtonItemHostObject>::default();
    env.objc.alloc_object(this, host_object, &mut env.mem)
}

- (id)initWithTitle:(id)title // NSString*
              style:(NSInteger)style
             target:(id)target
             action:(SEL)action {
    let title: id = msg![env; title copy];
    let host = env.objc.borrow_mut::<UIBarButtonItemHostObject>(this);
    host.title = title;
    host.style = style;
    host.target = target;
    host.action = (!action.is_null()).then_some(action);
    this
}

- (id)initWithBarButtonSystemItem:(UIBarButtonSystemItem)item
                           target:(id)target
                           action:(SEL)action {
    let title = get_static_str(env, system_item_title(item));
    let this: id = msg![env; this initWithTitle:title style:0 target:target action:action];
    env.objc.borrow_mut::<UIBarButtonItemHostObject>(this).system_item = Some(item);
    this
}

- (id)initWithCustomView:(id)view { // UIView*
    retain(env, view);
    env.objc.borrow_mut::<UIBarButtonItemHostObject>(this).custom_view = view;
    this
}

- (id)initWithCoder:(id)coder {
    let key = get_static_str(env, "UITitle");
    let title: id = msg![env; coder decodeObjectForKey:key];
    let key = get_static_str(env, "UIStyle");
    let style: NSInteger = msg![env; coder decodeIntegerForKey:key];
    let key = get_static_str(env, "UIIsSystemItem");
    let is_system: bool = msg![env; coder decodeBoolForKey:key];
    let system_item = if is_system {
        let key = get_static_str(env, "UISystemItem");
        let item: NSInteger = msg![env; coder decodeIntegerForKey:key];
        Some(item)
    } else {
        None
    };
    let title = match system_item {
        Some(item) if title == nil => get_static_str(env, system_item_title(item)),
        _ => title,
    };
    let key = get_static_str(env, "UIEnabled");
    let has_enabled: bool = msg![env; coder containsValueForKey:key];
    let enabled: bool = !has_enabled || msg![env; coder decodeBoolForKey:key];
    let key = get_static_str(env, "UICustomView");
    let custom_view: id = msg![env; coder decodeObjectForKey:key];
    let key = get_static_str(env, "UITag");
    let tag: NSInteger = msg![env; coder decodeIntegerForKey:key];
    let key = get_static_str(env, "UIWidth");
    let width: f32 = msg![env; coder decodeFloatForKey:key];

    let title: id = msg![env; title copy];
    retain(env, custom_view);
    let host = env.objc.borrow_mut::<UIBarButtonItemHostObject>(this);
    host.title = title;
    host.style = style;
    host.custom_view = custom_view;
    host.tag = tag;
    host.width = width;
    host.system_item = system_item;
    host.enabled = enabled;
    this
}

- (())dealloc {
    let &UIBarButtonItemHostObject { title, custom_view, .. } = env.objc.borrow(this);
    release(env, title);
    release(env, custom_view);
    env.objc.dealloc_object(this, &mut env.mem)
}

- (id)title {
    env.objc.borrow::<UIBarButtonItemHostObject>(this).title
}
- (())setTitle:(id)title { // NSString*
    let title: id = msg![env; title copy];
    let old = std::mem::replace(&mut env.objc.borrow_mut::<UIBarButtonItemHostObject>(this).title, title);
    release(env, old);
}
- (id)customView {
    env.objc.borrow::<UIBarButtonItemHostObject>(this).custom_view
}
- (())setCustomView:(id)view { // UIView*
    set_retained(env, |env| &mut env.objc.borrow_mut::<UIBarButtonItemHostObject>(this).custom_view, view);
}
- (id)target {
    env.objc.borrow::<UIBarButtonItemHostObject>(this).target
}
- (())setTarget:(id)target {
    env.objc.borrow_mut::<UIBarButtonItemHostObject>(this).target = target;
}
- (SEL)action {
    env.objc.borrow::<UIBarButtonItemHostObject>(this).action.unwrap_or_else(null_selector)
}
- (())setAction:(SEL)action {
    env.objc.borrow_mut::<UIBarButtonItemHostObject>(this).action = (!action.is_null()).then_some(action);
}
- (NSInteger)style {
    env.objc.borrow::<UIBarButtonItemHostObject>(this).style
}
- (())setStyle:(NSInteger)style {
    env.objc.borrow_mut::<UIBarButtonItemHostObject>(this).style = style;
}
- (bool)isEnabled {
    env.objc.borrow::<UIBarButtonItemHostObject>(this).enabled
}
- (())setEnabled:(bool)enabled {
    env.objc.borrow_mut::<UIBarButtonItemHostObject>(this).enabled = enabled;
}
- (CGFloat)width {
    env.objc.borrow::<UIBarButtonItemHostObject>(this).width
}
- (())setWidth:(CGFloat)width {
    env.objc.borrow_mut::<UIBarButtonItemHostObject>(this).width = width;
}
- (NSInteger)tag {
    env.objc.borrow::<UIBarButtonItemHostObject>(this).tag
}
- (())setTag:(NSInteger)tag {
    env.objc.borrow_mut::<UIBarButtonItemHostObject>(this).tag = tag;
}

// Nib files connect a bar button item's action like a control's.
- (())addTarget:(id)target
         action:(SEL)action
forControlEvents:(NSUInteger)_events {
    let host = env.objc.borrow_mut::<UIBarButtonItemHostObject>(this);
    host.target = target;
    host.action = (!action.is_null()).then_some(action);
}

// Called by the bar when the item's button is tapped.
- (())_touchHLE_sendAction {
    let &UIBarButtonItemHostObject { target, action, enabled, .. } = env.objc.borrow(this);
    let Some(action) = action else { return };
    if !enabled || target == nil {
        return;
    }
    if action.as_str(&env.mem).contains(':') {
        () = msg_send(env, (target, action, this));
    } else {
        () = msg_send(env, (target, action));
    }
}

@end

@implementation UINavigationItem: NSObject

+ (id)allocWithZone:(NSZonePtr)_zone {
    let host_object = Box::<UINavigationItemHostObject>::default();
    env.objc.alloc_object(this, host_object, &mut env.mem)
}

- (id)initWithTitle:(id)title { // NSString*
    () = msg![env; this setTitle:title];
    this
}

- (id)initWithCoder:(id)coder {
    for (key, setter) in [
        ("UITitle", "setTitle:"),
        ("UILeftBarButtonItem", "setLeftBarButtonItem:"),
        ("UIRightBarButtonItem", "setRightBarButtonItem:"),
        ("UIBackBarButtonItem", "setBackBarButtonItem:"),
        ("UITitleView", "setTitleView:"),
    ] {
        let key = get_static_str(env, key);
        let value: id = msg![env; coder decodeObjectForKey:key];
        if value != nil {
            let setter = env.objc.lookup_selector(setter).unwrap();
            () = msg_send(env, (this, setter, value));
        }
    }
    let key = get_static_str(env, "UIHidesBackButton");
    let hides: bool = msg![env; coder decodeBoolForKey:key];
    env.objc.borrow_mut::<UINavigationItemHostObject>(this).hides_back_button = hides;
    this
}

- (())dealloc {
    let &UINavigationItemHostObject { title, left, right, back, title_view, .. } = env.objc.borrow(this);
    for object in [title, left, right, back, title_view] {
        release(env, object);
    }
    env.objc.dealloc_object(this, &mut env.mem)
}

- (id)title {
    env.objc.borrow::<UINavigationItemHostObject>(this).title
}
- (())setTitle:(id)title { // NSString*
    let title: id = msg![env; title copy];
    let old = std::mem::replace(&mut env.objc.borrow_mut::<UINavigationItemHostObject>(this).title, title);
    release(env, old);
    update_bar_of_item(env, this);
}
- (id)leftBarButtonItem {
    env.objc.borrow::<UINavigationItemHostObject>(this).left
}
- (())setLeftBarButtonItem:(id)item { // UIBarButtonItem*
    set_retained(env, |env| &mut env.objc.borrow_mut::<UINavigationItemHostObject>(this).left, item);
    update_bar_of_item(env, this);
}
- (())setLeftBarButtonItem:(id)item animated:(bool)_animated {
    () = msg![env; this setLeftBarButtonItem:item];
}
- (id)rightBarButtonItem {
    env.objc.borrow::<UINavigationItemHostObject>(this).right
}
- (())setRightBarButtonItem:(id)item { // UIBarButtonItem*
    set_retained(env, |env| &mut env.objc.borrow_mut::<UINavigationItemHostObject>(this).right, item);
    update_bar_of_item(env, this);
}
- (())setRightBarButtonItem:(id)item animated:(bool)_animated {
    () = msg![env; this setRightBarButtonItem:item];
}
- (id)backBarButtonItem {
    env.objc.borrow::<UINavigationItemHostObject>(this).back
}
- (())setBackBarButtonItem:(id)item { // UIBarButtonItem*
    set_retained(env, |env| &mut env.objc.borrow_mut::<UINavigationItemHostObject>(this).back, item);
    update_bar_of_item(env, this);
}
- (id)titleView {
    env.objc.borrow::<UINavigationItemHostObject>(this).title_view
}
- (())setTitleView:(id)view { // UIView*
    set_retained(env, |env| &mut env.objc.borrow_mut::<UINavigationItemHostObject>(this).title_view, view);
    update_bar_of_item(env, this);
}
- (bool)hidesBackButton {
    env.objc.borrow::<UINavigationItemHostObject>(this).hides_back_button
}
- (())setHidesBackButton:(bool)hides {
    env.objc.borrow_mut::<UINavigationItemHostObject>(this).hides_back_button = hides;
    update_bar_of_item(env, this);
}
- (())setHidesBackButton:(bool)hides animated:(bool)_animated {
    () = msg![env; this setHidesBackButton:hides];
}
- (id)prompt {
    nil
}
- (())setPrompt:(id)_prompt {
    // TODO: show the prompt above the title
}

@end

@implementation UINavigationBar: UIView

+ (id)allocWithZone:(NSZonePtr)_zone {
    let host_object = Box::<UINavigationBarHostObject>::default();
    env.objc.alloc_object(this, host_object, &mut env.mem)
}

- (id)initWithFrame:(CGRect)frame {
    let this: id = msg_super![env; this initWithFrame:frame];
    set_up_subviews(env, this);
    this
}

- (id)initWithCoder:(id)coder {
    let this: id = msg_super![env; this initWithCoder:coder];
    set_up_subviews(env, this);

    let key = get_static_str(env, "UIBarStyle");
    let bar_style: NSInteger = msg![env; coder decodeIntegerForKey:key];
    env.objc.borrow_mut::<UINavigationBarHostObject>(this).bar_style = bar_style;
    let key = get_static_str(env, "UITintColor");
    let tint_color: id = msg![env; coder decodeObjectForKey:key];
    () = msg![env; this setTintColor:tint_color];
    let key = get_static_str(env, "UIItems");
    let items: id = msg![env; coder decodeObjectForKey:key];
    if items != nil {
        () = msg![env; this setItems:items];
    }
    () = msg![env; this layoutSubviews];
    this
}

- (())dealloc {
    let host = env.objc.borrow_mut::<UINavigationBarHostObject>(this);
    let items = std::mem::take(&mut host.items);
    let objects = [host.tint_color, host.title_label, host.left_button, host.right_button];
    for item in items {
        env.objc.borrow_mut::<UINavigationItemHostObject>(item).bar = nil;
        release(env, item);
    }
    for object in objects {
        release(env, object);
    }
    msg_super![env; this dealloc]
}

- (id)items {
    let items = env.objc.borrow::<UINavigationBarHostObject>(this).items.clone();
    for &item in &items {
        retain(env, item);
    }
    let array = crate::frameworks::foundation::ns_array::from_vec(env, items);
    autorelease(env, array)
}
- (())setItems:(id)items { // NSArray*
    let count: NSUInteger = msg![env; items count];
    let mut new_items = Vec::new();
    for i in 0..count {
        let item: id = msg![env; items objectAtIndex:i];
        retain(env, item);
        new_items.push(item);
    }
    let old_items = std::mem::replace(&mut env.objc.borrow_mut::<UINavigationBarHostObject>(this).items, new_items.clone());
    for item in old_items {
        if env.objc.borrow::<UINavigationItemHostObject>(item).bar == this {
            env.objc.borrow_mut::<UINavigationItemHostObject>(item).bar = nil;
        }
        release(env, item);
    }
    for item in new_items {
        env.objc.borrow_mut::<UINavigationItemHostObject>(item).bar = this;
    }
    () = msg![env; this layoutSubviews];
}
- (())setItems:(id)items animated:(bool)_animated {
    () = msg![env; this setItems:items];
}
- (())pushNavigationItem:(id)item animated:(bool)_animated { // UINavigationItem*
    retain(env, item);
    env.objc.borrow_mut::<UINavigationItemHostObject>(item).bar = this;
    env.objc.borrow_mut::<UINavigationBarHostObject>(this).items.push(item);
    () = msg![env; this layoutSubviews];
}
- (id)popNavigationItemAnimated:(bool)_animated {
    let Some(item) = env.objc.borrow_mut::<UINavigationBarHostObject>(this).items.pop() else {
        return nil;
    };
    env.objc.borrow_mut::<UINavigationItemHostObject>(item).bar = nil;
    () = msg![env; this layoutSubviews];
    autorelease(env, item)
}
- (id)topItem {
    env.objc.borrow::<UINavigationBarHostObject>(this).items.last().copied().unwrap_or(nil)
}
- (id)backItem {
    let items = &env.objc.borrow::<UINavigationBarHostObject>(this).items;
    if items.len() >= 2 { items[items.len() - 2] } else { nil }
}

- (UIBarStyle)barStyle {
    env.objc.borrow::<UINavigationBarHostObject>(this).bar_style
}
- (())setBarStyle:(UIBarStyle)style {
    env.objc.borrow_mut::<UINavigationBarHostObject>(this).bar_style = style;
    () = msg![env; this layoutSubviews];
}
- (id)tintColor {
    env.objc.borrow::<UINavigationBarHostObject>(this).tint_color
}
- (())setTintColor:(id)color { // UIColor*
    set_retained(env, |env| &mut env.objc.borrow_mut::<UINavigationBarHostObject>(this).tint_color, color);
    () = msg![env; this layoutSubviews];
}
- (())setTranslucent:(bool)_translucent {}
- (id)delegate {
    env.objc.borrow::<UINavigationBarHostObject>(this).delegate
}
- (())setDelegate:(id)delegate {
    env.objc.borrow_mut::<UINavigationBarHostObject>(this).delegate = delegate;
}

- (CGSize)sizeThatFits:(CGSize)size {
    CGSize { width: size.width, height: BAR_HEIGHT }
}

- (())layoutSubviews {
    lay_out(env, this);
}

- (())_touchHLE_leftTapped:(id)_sender {
    let top: id = msg![env; this topItem];
    if top == nil {
        return;
    }
    let left = env.objc.borrow::<UINavigationItemHostObject>(top).left;
    if left != nil {
        () = msg![env; left _touchHLE_sendAction];
        return;
    }
    // The back button.
    let delegate = env.objc.borrow::<UINavigationBarHostObject>(this).delegate;
    if delegate != nil {
        let sel = env.objc.lookup_selector("navigationBar:shouldPopItem:");
        if let Some(sel) = sel {
            let responds: bool = msg![env; delegate respondsToSelector:sel];
            if responds {
                let pop: bool = msg_send(env, (delegate, sel, this, top));
                if !pop {
                    return;
                }
            }
        }
    }
    retain(env, top);
    let _: id = msg![env; this popNavigationItemAnimated:false];
    if delegate != nil {
        if let Some(sel) = env.objc.lookup_selector("navigationBar:didPopItem:") {
            let responds: bool = msg![env; delegate respondsToSelector:sel];
            if responds {
                () = msg_send(env, (delegate, sel, this, top));
            }
        }
    }
    release(env, top);
}

- (())_touchHLE_rightTapped:(id)_sender {
    let top: id = msg![env; this topItem];
    if top == nil {
        return;
    }
    let right = env.objc.borrow::<UINavigationItemHostObject>(top).right;
    if right != nil {
        () = msg![env; right _touchHLE_sendAction];
    }
}

@end

@implementation UIToolbar: UIView

+ (id)allocWithZone:(NSZonePtr)_zone {
    let host_object = Box::<UIToolbarHostObject>::default();
    env.objc.alloc_object(this, host_object, &mut env.mem)
}

- (id)initWithCoder:(id)coder {
    let this: id = msg_super![env; this initWithCoder:coder];
    let key = get_static_str(env, "UIBarStyle");
    let bar_style: NSInteger = msg![env; coder decodeIntegerForKey:key];
    env.objc.borrow_mut::<UIToolbarHostObject>(this).bar_style = bar_style;
    let key = get_static_str(env, "UIItems");
    let items: id = msg![env; coder decodeObjectForKey:key];
    () = msg![env; this setItems:items];
    this
}

- (())dealloc {
    let host = env.objc.borrow_mut::<UIToolbarHostObject>(this);
    let items = std::mem::take(&mut host.items);
    let buttons = std::mem::take(&mut host.buttons);
    let tint_color = host.tint_color;
    for object in items.into_iter().chain(buttons.into_iter().map(|(button, _)| button)) {
        release(env, object);
    }
    release(env, tint_color);
    msg_super![env; this dealloc]
}

- (id)items {
    let items = env.objc.borrow::<UIToolbarHostObject>(this).items.clone();
    for &item in &items {
        retain(env, item);
    }
    let array = crate::frameworks::foundation::ns_array::from_vec(env, items);
    autorelease(env, array)
}
- (())setItems:(id)items { // NSArray*
    let mut new_items = Vec::new();
    if items != nil {
        let count: NSUInteger = msg![env; items count];
        for i in 0..count {
            let item: id = msg![env; items objectAtIndex:i];
            retain(env, item);
            new_items.push(item);
        }
    }
    let old = std::mem::replace(&mut env.objc.borrow_mut::<UIToolbarHostObject>(this).items, new_items);
    for item in old {
        release(env, item);
    }
    () = msg![env; this layoutSubviews];
}
- (())setItems:(id)items animated:(bool)_animated {
    () = msg![env; this setItems:items];
}
- (NSInteger)barStyle {
    env.objc.borrow::<UIToolbarHostObject>(this).bar_style
}
- (())setBarStyle:(NSInteger)style {
    env.objc.borrow_mut::<UIToolbarHostObject>(this).bar_style = style;
    () = msg![env; this layoutSubviews];
}
- (id)tintColor {
    env.objc.borrow::<UIToolbarHostObject>(this).tint_color
}
- (())setTintColor:(id)color { // UIColor*
    set_retained(env, |env| &mut env.objc.borrow_mut::<UIToolbarHostObject>(this).tint_color, color);
    () = msg![env; this layoutSubviews];
}
- (())setTranslucent:(bool)_translucent {}

- (())setFrame:(CGRect)frame {
    () = msg_super![env; this setFrame:frame];
    () = msg![env; this layoutSubviews];
}
- (())layoutSubviews {
    lay_out_toolbar(env, this);
}

- (())_touchHLE_itemTapped:(id)sender {
    let item = env
        .objc
        .borrow::<UIToolbarHostObject>(this)
        .buttons
        .iter()
        .find(|&&(button, _)| button == sender)
        .map(|&(_, item)| item);
    if let Some(item) = item {
        () = msg![env; item _touchHLE_sendAction];
    }
}

@end

};

/// Create the title label and the two buttons.
fn set_up_subviews(env: &mut Environment, bar: id) {
    let label: id = msg_class![env; UILabel alloc];
    let label: id = msg![env; label init];
    let font: id = msg_class![env; UIFont boldSystemFontOfSize:(20.0 as CGFloat)];
    () = msg![env; label setFont:font];
    let white: id = msg_class![env; UIColor whiteColor];
    () = msg![env; label setTextColor:white];
    let clear: id = msg_class![env; UIColor clearColor];
    () = msg![env; label setBackgroundColor:clear];
    () = msg![env; label setTextAlignment:UITextAlignmentCenter];
    () = msg![env; bar addSubview:label];

    let mut buttons = [nil; 2];
    for (button, action) in buttons
        .iter_mut()
        .zip(["_touchHLE_leftTapped:", "_touchHLE_rightTapped:"])
    {
        let new: id = msg_class![env; UIButton buttonWithType:UIButtonTypeCustom];
        retain(env, new);
        let font: id = msg_class![env; UIFont boldSystemFontOfSize:(12.0 as CGFloat)];
        () = msg![env; new setFont:font];
        () = msg![env; new setTitleColor:white forState:UIControlStateNormal];
        let action = env.objc.lookup_selector(action).unwrap();
        () =
            msg![env; new addTarget:bar action:action forControlEvents:UIControlEventTouchUpInside];
        () = msg![env; bar addSubview:new];
        *button = new;
    }

    let host = env.objc.borrow_mut::<UINavigationBarHostObject>(bar);
    host.title_label = label;
    host.left_button = buttons[0];
    host.right_button = buttons[1];
}

/// Show the top item: its title, and its buttons (or a back button).
fn lay_out(env: &mut Environment, bar: id) {
    let &UINavigationBarHostObject {
        bar_style,
        tint_color,
        title_label,
        left_button,
        right_button,
        ..
    } = env.objc.borrow(bar);
    if title_label == nil {
        return; // not set up yet
    }

    let background: id = if tint_color != nil {
        tint_color
    } else if bar_style == UIBarStyleDefault {
        msg_class![env; UIColor colorWithRed:(0.43 as CGFloat) green:(0.52 as CGFloat) blue:(0.64 as CGFloat) alpha:(1.0 as CGFloat)]
    } else {
        msg_class![env; UIColor colorWithRed:(0.1 as CGFloat) green:(0.1 as CGFloat) blue:(0.1 as CGFloat) alpha:(1.0 as CGFloat)]
    };
    () = msg![env; bar setBackgroundColor:background];
    let button_color: id = msg![env; background colorWithAlphaComponent:(0.6 as CGFloat)];

    // Remove custom views shown for the previous item.
    let old_views = std::mem::take(
        &mut env
            .objc
            .borrow_mut::<UINavigationBarHostObject>(bar)
            .shown_custom_views,
    );
    for view in old_views {
        () = msg![env; view removeFromSuperview];
    }

    let bounds: CGRect = msg![env; bar bounds];
    let width = bounds.size.width;
    let height = bounds.size.height;

    let top: id = msg![env; bar topItem];
    let back: id = msg![env; bar backItem];
    let (title, title_view, left, right, hides_back) = if top == nil {
        (nil, nil, nil, nil, true)
    } else {
        let host = env.objc.borrow::<UINavigationItemHostObject>(top);
        (
            host.title,
            host.title_view,
            host.left,
            host.right,
            host.hides_back_button,
        )
    };

    // The left side: the item's own button, or a back button.
    let left_title: id = if left != nil {
        env.objc.borrow::<UIBarButtonItemHostObject>(left).title
    } else if back != nil && !hides_back {
        let back_item = env.objc.borrow::<UINavigationItemHostObject>(back).back;
        let back_title = if back_item != nil {
            env.objc
                .borrow::<UIBarButtonItemHostObject>(back_item)
                .title
        } else {
            env.objc.borrow::<UINavigationItemHostObject>(back).title
        };
        if back_title == nil {
            get_static_str(env, "Back")
        } else {
            back_title
        }
    } else {
        nil
    };
    let left_shown = left != nil || left_title != nil;
    let left_width = place_button(
        env,
        bar,
        left_button,
        left,
        left_title,
        left_shown,
        button_color,
        true,
        width,
        height,
    );

    let right_title = if right != nil {
        env.objc.borrow::<UIBarButtonItemHostObject>(right).title
    } else {
        nil
    };
    let right_width = place_button(
        env,
        bar,
        right_button,
        right,
        right_title,
        right != nil,
        button_color,
        false,
        width,
        height,
    );

    // The title, centred in the space between the buttons.
    let margin = left_width.max(right_width) + 10.0;
    let title_frame = CGRect {
        origin: CGPoint { x: margin, y: 0.0 },
        size: CGSize {
            width: (width - 2.0 * margin).max(0.0),
            height,
        },
    };
    if title_view != nil {
        () = msg![env; title_label setHidden:true];
        let view_size: CGSize = {
            let bounds: CGRect = msg![env; title_view bounds];
            bounds.size
        };
        let frame = CGRect {
            origin: CGPoint {
                x: ((width - view_size.width) / 2.0).round(),
                y: ((height - view_size.height) / 2.0).round(),
            },
            size: view_size,
        };
        () = msg![env; title_view setFrame:frame];
        () = msg![env; bar addSubview:title_view];
        env.objc
            .borrow_mut::<UINavigationBarHostObject>(bar)
            .shown_custom_views
            .push(title_view);
    } else {
        () = msg![env; title_label setHidden:false];
        () = msg![env; title_label setText:title];
        () = msg![env; title_label setFrame:title_frame];
    }
}

/// Show one side's bar button item (or hide its button) and return the
/// width it takes up.
#[allow(clippy::too_many_arguments)]
fn place_button(
    env: &mut Environment,
    bar: id,
    button: id,
    item: id,
    title: id,
    shown: bool,
    color: id,
    left: bool,
    bar_width: CGFloat,
    bar_height: CGFloat,
) -> CGFloat {
    let custom_view = if item != nil {
        env.objc
            .borrow::<UIBarButtonItemHostObject>(item)
            .custom_view
    } else {
        nil
    };
    if !shown || custom_view != nil {
        () = msg![env; button setHidden:true];
    }
    if !shown {
        return 0.0;
    }
    let button_height: CGFloat = 30.0;
    let y = ((bar_height - button_height) / 2.0).round();
    if custom_view != nil {
        let bounds: CGRect = msg![env; custom_view bounds];
        let x = if left {
            5.0
        } else {
            bar_width - 5.0 - bounds.size.width
        };
        let frame = CGRect {
            origin: CGPoint {
                x,
                y: ((bar_height - bounds.size.height) / 2.0).round(),
            },
            size: bounds.size,
        };
        () = msg![env; custom_view setFrame:frame];
        () = msg![env; bar addSubview:custom_view];
        env.objc
            .borrow_mut::<UINavigationBarHostObject>(bar)
            .shown_custom_views
            .push(custom_view);
        return 5.0 + bounds.size.width;
    }
    let text = if title == nil {
        String::new()
    } else {
        to_rust_string(env, title).to_string()
    };
    let fixed_width = if item != nil {
        env.objc.borrow::<UIBarButtonItemHostObject>(item).width
    } else {
        0.0
    };
    let button_width = if fixed_width > 0.0 {
        fixed_width
    } else {
        (text.chars().count() as CGFloat * 8.0 + 20.0).max(50.0)
    };
    let x = if left {
        5.0
    } else {
        bar_width - 5.0 - button_width
    };
    let frame = CGRect {
        origin: CGPoint { x, y },
        size: CGSize {
            width: button_width,
            height: button_height,
        },
    };
    let title = from_rust_string(env, text);
    () = msg![env; button setTitle:title forState:UIControlStateNormal];
    release(env, title);
    () = msg![env; button setBackgroundColor:color];
    () = msg![env; button setFrame:frame];
    () = msg![env; button setHidden:false];
    5.0 + button_width
}

/// Lay out the toolbar's items from left to right: buttons (or custom views)
/// with flexible spaces sharing out the room that is left.
fn lay_out_toolbar(env: &mut Environment, toolbar: id) {
    let old_buttons =
        std::mem::take(&mut env.objc.borrow_mut::<UIToolbarHostObject>(toolbar).buttons);
    for (button, _) in old_buttons {
        () = msg![env; button removeFromSuperview];
        release(env, button);
    }
    let &UIToolbarHostObject {
        bar_style,
        tint_color,
        ..
    } = env.objc.borrow(toolbar);
    let items = env
        .objc
        .borrow::<UIToolbarHostObject>(toolbar)
        .items
        .clone();

    let background: id = if tint_color != nil {
        tint_color
    } else if bar_style == UIBarStyleDefault {
        msg_class![env; UIColor colorWithRed:(0.43 as CGFloat) green:(0.52 as CGFloat) blue:(0.64 as CGFloat) alpha:(1.0 as CGFloat)]
    } else {
        msg_class![env; UIColor colorWithRed:(0.1 as CGFloat) green:(0.1 as CGFloat) blue:(0.1 as CGFloat) alpha:(1.0 as CGFloat)]
    };
    () = msg![env; toolbar setBackgroundColor:background];
    let button_color: id = msg![env; background colorWithAlphaComponent:(0.6 as CGFloat)];
    let white: id = msg_class![env; UIColor whiteColor];

    let bounds: CGRect = msg![env; toolbar bounds];
    let (width, height) = (bounds.size.width, bounds.size.height);
    let gap: CGFloat = 6.0;

    // Widths: fixed for buttons and custom views, shared for flexible space.
    let mut widths = Vec::new();
    let mut flexible = 0;
    for &item in &items {
        let host = env.objc.borrow::<UIBarButtonItemHostObject>(item);
        let (system_item, custom_view, title, width) =
            (host.system_item, host.custom_view, host.title, host.width);
        let w = if system_item == Some(UIBarButtonSystemItemFlexibleSpace) {
            flexible += 1;
            0.0
        } else if system_item == Some(UIBarButtonSystemItemFixedSpace) {
            width
        } else if custom_view != nil {
            let view_bounds: CGRect = msg![env; custom_view bounds];
            view_bounds.size.width
        } else if width > 0.0 {
            width
        } else {
            let text = if title == nil {
                String::new()
            } else {
                to_rust_string(env, title).to_string()
            };
            (text.chars().count() as CGFloat * 8.0 + 20.0).max(40.0)
        };
        widths.push(w);
    }
    let used: CGFloat = widths.iter().sum::<CGFloat>() + gap * (items.len() as CGFloat + 1.0);
    let flexible_width = if flexible > 0 {
        ((width - used) / flexible as CGFloat).max(0.0)
    } else {
        0.0
    };

    let mut x = gap;
    for (&item, &w) in items.iter().zip(&widths) {
        let host = env.objc.borrow::<UIBarButtonItemHostObject>(item);
        let (system_item, custom_view, title) = (host.system_item, host.custom_view, host.title);
        if system_item == Some(UIBarButtonSystemItemFlexibleSpace) {
            x += flexible_width + gap;
            continue;
        }
        if system_item == Some(UIBarButtonSystemItemFixedSpace) {
            x += w + gap;
            continue;
        }
        if custom_view != nil {
            let view_bounds: CGRect = msg![env; custom_view bounds];
            let frame = CGRect {
                origin: CGPoint {
                    x,
                    y: ((height - view_bounds.size.height) / 2.0).round(),
                },
                size: view_bounds.size,
            };
            () = msg![env; custom_view setFrame:frame];
            () = msg![env; toolbar addSubview:custom_view];
            x += w + gap;
            continue;
        }
        let button: id = msg_class![env; UIButton buttonWithType:UIButtonTypeCustom];
        retain(env, button);
        let font: id = msg_class![env; UIFont boldSystemFontOfSize:(12.0 as CGFloat)];
        () = msg![env; button setFont:font];
        () = msg![env; button setTitleColor:white forState:UIControlStateNormal];
        () = msg![env; button setTitle:title forState:UIControlStateNormal];
        () = msg![env; button setBackgroundColor:button_color];
        let frame = CGRect {
            origin: CGPoint {
                x,
                y: ((height - 30.0) / 2.0).round(),
            },
            size: CGSize {
                width: w,
                height: 30.0,
            },
        };
        () = msg![env; button setFrame:frame];
        let action = env.objc.lookup_selector("_touchHLE_itemTapped:").unwrap();
        () = msg![env; button addTarget:toolbar action:action forControlEvents:UIControlEventTouchUpInside];
        () = msg![env; toolbar addSubview:button];
        env.objc
            .borrow_mut::<UIToolbarHostObject>(toolbar)
            .buttons
            .push((button, item));
        x += w + gap;
    }
}
