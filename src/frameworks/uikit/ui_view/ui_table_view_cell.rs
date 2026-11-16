/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `UITableViewCell`.
//!
//! Supports the four standard styles, the iPhone OS 2.x properties (`text`,
//! `image`, `font` etc., which set those of the labels and image view) and
//! the standard accessories, drawn as simple characters.

use crate::frameworks::core_graphics::{CGFloat, CGPoint, CGRect, CGSize};
use crate::frameworks::foundation::ns_string::get_static_str;
use crate::frameworks::foundation::NSInteger;
use crate::frameworks::uikit::ui_font::UITextAlignmentRight;
use crate::objc::{
    id, impl_HostObject_with_superclass, msg, msg_class, msg_super, nil, objc_classes, release,
    retain, ClassExports, NSZonePtr,
};
use crate::Environment;

type UITableViewCellStyle = NSInteger;
const UITableViewCellStyleDefault: UITableViewCellStyle = 0;
const UITableViewCellStyleValue1: UITableViewCellStyle = 1;
const UITableViewCellStyleValue2: UITableViewCellStyle = 2;
const UITableViewCellStyleSubtitle: UITableViewCellStyle = 3;

type UITableViewCellAccessoryType = NSInteger;
const UITableViewCellAccessoryNone: UITableViewCellAccessoryType = 0;
const UITableViewCellAccessoryDisclosureIndicator: UITableViewCellAccessoryType = 1;
const UITableViewCellAccessoryDetailDisclosureButton: UITableViewCellAccessoryType = 2;
const UITableViewCellAccessoryCheckmark: UITableViewCellAccessoryType = 3;

type UITableViewCellSelectionStyle = NSInteger;
const UITableViewCellSelectionStyleNone: UITableViewCellSelectionStyle = 0;

/// Width taken by an accessory at the right of the cell.
pub const ACCESSORY_WIDTH: CGFloat = 30.0;

#[derive(Default)]
pub struct UITableViewCellHostObject {
    superclass: super::UIViewHostObject,
    style: UITableViewCellStyle,
    /// `NSString*`
    reuse_identifier: id,
    /// The views below are retained and are also subviews.
    content_view: id,
    text_label: id,
    detail_text_label: id,
    image_view: id,
    /// Shows the standard accessories.
    accessory_label: id,
    /// `UIView*`, set by the app.
    accessory_view: id,
    /// `UIView*`s, set by the app.
    background_view: id,
    selected_background_view: id,
    accessory_type: UITableViewCellAccessoryType,
    selection_style: UITableViewCellSelectionStyle,
    selected: bool,
    /// `UIColor*`, set with the iPhone OS 2.x `selectedTextColor`.
    selected_text_color: id,
}
impl_HostObject_with_superclass!(UITableViewCellHostObject);

pub fn reuse_identifier(env: &Environment, cell: id) -> id {
    env.objc
        .borrow::<UITableViewCellHostObject>(cell)
        .reuse_identifier
}
pub fn accessory_type(env: &Environment, cell: id) -> UITableViewCellAccessoryType {
    env.objc
        .borrow::<UITableViewCellHostObject>(cell)
        .accessory_type
}
pub fn is_detail_disclosure(accessory_type: UITableViewCellAccessoryType) -> bool {
    accessory_type == UITableViewCellAccessoryDetailDisclosureButton
}

fn host(env: &mut Environment, cell: id) -> &mut UITableViewCellHostObject {
    env.objc.borrow_mut(cell)
}

fn replace_retained(
    env: &mut Environment,
    cell: id,
    slot: fn(&mut UITableViewCellHostObject) -> &mut id,
    new: id,
) -> id {
    retain(env, new);
    let old = std::mem::replace(slot(host(env, cell)), new);
    release(env, old);
    old
}

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

@implementation UITableViewCell: UIView

+ (id)allocWithZone:(NSZonePtr)_zone {
    let host_object = Box::<UITableViewCellHostObject>::default();
    env.objc.alloc_object(this, host_object, &mut env.mem)
}

- (id)initWithStyle:(UITableViewCellStyle)style
    reuseIdentifier:(id)reuse_identifier { // NSString*
    let frame = CGRect {
        origin: CGPoint { x: 0.0, y: 0.0 },
        size: CGSize { width: 320.0, height: 44.0 },
    };
    let this: id = msg_super![env; this initWithFrame:frame];
    set_up(env, this, style, reuse_identifier);
    this
}

// iPhone OS 2.x
- (id)initWithFrame:(CGRect)frame
    reuseIdentifier:(id)reuse_identifier { // NSString*
    let this: id = msg_super![env; this initWithFrame:frame];
    set_up(env, this, UITableViewCellStyleDefault, reuse_identifier);
    this
}

- (id)initWithFrame:(CGRect)frame {
    let this: id = msg_super![env; this initWithFrame:frame];
    set_up(env, this, UITableViewCellStyleDefault, nil);
    this
}

- (id)initWithCoder:(id)coder {
    let this: id = msg_super![env; this initWithCoder:coder];
    let key = get_static_str(env, "UIReuseIdentifier");
    let reuse_identifier: id = msg![env; coder decodeObjectForKey:key];
    set_up(env, this, UITableViewCellStyleDefault, reuse_identifier);
    this
}

- (())dealloc {
    let host = host(env, this);
    let objects = [
        host.reuse_identifier,
        host.content_view,
        host.text_label,
        host.detail_text_label,
        host.image_view,
        host.accessory_label,
        host.accessory_view,
        host.background_view,
        host.selected_background_view,
        host.selected_text_color,
    ];
    for object in objects {
        release(env, object);
    }
    msg_super![env; this dealloc]
}

- (id)reuseIdentifier {
    host(env, this).reuse_identifier
}
- (())prepareForReuse {
    () = msg![env; this setSelected:false];
}

- (id)contentView {
    host(env, this).content_view
}
- (id)textLabel {
    host(env, this).text_label
}
- (id)detailTextLabel {
    host(env, this).detail_text_label
}
- (id)imageView {
    host(env, this).image_view
}

- (UITableViewCellAccessoryType)accessoryType {
    host(env, this).accessory_type
}
- (())setAccessoryType:(UITableViewCellAccessoryType)accessory_type {
    host(env, this).accessory_type = accessory_type;
    () = msg![env; this layoutSubviews];
}
- (())setEditingAccessoryType:(UITableViewCellAccessoryType)_accessory_type {}
- (id)accessoryView {
    host(env, this).accessory_view
}
- (())setAccessoryView:(id)view { // UIView*
    let old = replace_retained(env, this, |h| &mut h.accessory_view, view);
    if old != nil {
        () = msg![env; old removeFromSuperview];
    }
    if view != nil {
        () = msg![env; this addSubview:view];
    }
    () = msg![env; this layoutSubviews];
}
- (id)backgroundView {
    host(env, this).background_view
}
- (())setBackgroundView:(id)view { // UIView*
    let old = replace_retained(env, this, |h| &mut h.background_view, view);
    if old != nil {
        () = msg![env; old removeFromSuperview];
    }
    if view != nil {
        () = msg![env; this insertSubview:view atIndex:0i32];
    }
    () = msg![env; this layoutSubviews];
}
- (id)selectedBackgroundView {
    host(env, this).selected_background_view
}
- (())setSelectedBackgroundView:(id)view { // UIView*
    replace_retained(env, this, |h| &mut h.selected_background_view, view);
}
- (UITableViewCellSelectionStyle)selectionStyle {
    host(env, this).selection_style
}
- (())setSelectionStyle:(UITableViewCellSelectionStyle)style {
    host(env, this).selection_style = style;
}
- (())setShowsReorderControl:(bool)_shows {}
- (())setHidesAccessoryWhenEditing:(bool)_hides {}
- (())setIndentationLevel:(NSInteger)_level {}
- (())setIndentationWidth:(CGFloat)_width {}

- (bool)isSelected {
    host(env, this).selected
}
- (())setSelected:(bool)selected {
    host(env, this).selected = selected;
    show_selection(env, this);
}
- (())setSelected:(bool)selected animated:(bool)_animated {
    () = msg![env; this setSelected:selected];
}
- (bool)isHighlighted {
    host(env, this).selected
}
- (())setHighlighted:(bool)highlighted {
    () = msg![env; this setSelected:highlighted];
}
- (())setHighlighted:(bool)highlighted animated:(bool)_animated {
    () = msg![env; this setSelected:highlighted];
}

// iPhone OS 2.x properties, which are those of the labels and image view.
- (id)text {
    let label = host(env, this).text_label;
    msg![env; label text]
}
- (())setText:(id)text { // NSString*
    let label = host(env, this).text_label;
    () = msg![env; label setText:text];
}
- (id)font {
    let label = host(env, this).text_label;
    msg![env; label font]
}
- (())setFont:(id)font { // UIFont*
    let label = host(env, this).text_label;
    () = msg![env; label setFont:font];
}
- (id)textColor {
    let label = host(env, this).text_label;
    msg![env; label textColor]
}
- (())setTextColor:(id)color { // UIColor*
    let label = host(env, this).text_label;
    () = msg![env; label setTextColor:color];
}
- (())setSelectedTextColor:(id)color { // UIColor*
    replace_retained(env, this, |h| &mut h.selected_text_color, color);
}
- (())setTextAlignment:(NSInteger)alignment {
    let label = host(env, this).text_label;
    () = msg![env; label setTextAlignment:alignment];
}
- (())setLineBreakMode:(NSInteger)mode {
    let label = host(env, this).text_label;
    () = msg![env; label setLineBreakMode:mode];
}
- (id)image {
    let image_view = host(env, this).image_view;
    msg![env; image_view image]
}
- (())setImage:(id)image { // UIImage*
    let image_view = host(env, this).image_view;
    () = msg![env; image_view setImage:image];
    () = msg![env; this layoutSubviews];
}

- (())setFrame:(CGRect)frame {
    () = msg_super![env; this setFrame:frame];
    () = msg![env; this layoutSubviews];
}

- (())layoutSubviews {
    lay_out(env, this);
}

@end

};

fn new_label(env: &mut Environment, font_size: CGFloat, bold: bool) -> id {
    let label: id = msg_class![env; UILabel alloc];
    let label: id = msg![env; label init];
    let font: id = if bold {
        msg_class![env; UIFont boldSystemFontOfSize:font_size]
    } else {
        msg_class![env; UIFont systemFontOfSize:font_size]
    };
    () = msg![env; label setFont:font];
    let clear: id = msg_class![env; UIColor clearColor];
    () = msg![env; label setBackgroundColor:clear];
    label
}

fn set_up(env: &mut Environment, cell: id, style: UITableViewCellStyle, reuse_identifier: id) {
    let reuse_identifier: id = msg![env; reuse_identifier copy];

    let bounds: CGRect = msg![env; cell bounds];
    let content_view: id = msg_class![env; UIView alloc];
    let content_view: id = msg![env; content_view initWithFrame:bounds];
    let clear: id = msg_class![env; UIColor clearColor];
    () = msg![env; content_view setBackgroundColor:clear];
    () = msg![env; cell addSubview:content_view];

    let image_view: id = msg_class![env; UIImageView alloc];
    let image_view: id = msg![env; image_view init];
    () = msg![env; content_view addSubview:image_view];

    let text_label = new_label(
        env,
        if style == UITableViewCellStyleDefault {
            20.0
        } else {
            17.0
        },
        true,
    );
    () = msg![env; content_view addSubview:text_label];

    let detail_text_label = new_label(env, 14.0, false);
    let detail_color: id = if style == UITableViewCellStyleValue2 {
        msg_class![env; UIColor colorWithRed:(0.32 as CGFloat) green:(0.4 as CGFloat) blue:(0.57 as CGFloat) alpha:(1.0 as CGFloat)]
    } else {
        msg_class![env; UIColor grayColor]
    };
    () = msg![env; detail_text_label setTextColor:detail_color];
    if style == UITableViewCellStyleValue1 {
        () = msg![env; detail_text_label setTextAlignment:UITextAlignmentRight];
    }
    () = msg![env; content_view addSubview:detail_text_label];

    let accessory_label = new_label(env, 20.0, true);
    () = msg![env; cell addSubview:accessory_label];

    let white: id = msg_class![env; UIColor whiteColor];
    () = msg![env; cell setBackgroundColor:white];

    let host = host(env, cell);
    host.style = style;
    host.reuse_identifier = reuse_identifier;
    host.content_view = content_view;
    host.text_label = text_label;
    host.detail_text_label = detail_text_label;
    host.image_view = image_view;
    host.accessory_label = accessory_label;
    host.selection_style = 1; // blue

    lay_out(env, cell);
}

fn lay_out(env: &mut Environment, cell: id) {
    let &mut UITableViewCellHostObject {
        style,
        content_view,
        text_label,
        detail_text_label,
        image_view,
        accessory_label,
        accessory_view,
        background_view,
        accessory_type,
        ..
    } = host(env, cell);
    if content_view == nil {
        return; // not set up yet
    }
    let bounds: CGRect = msg![env; cell bounds];
    let (width, height) = (bounds.size.width, bounds.size.height);
    let rect = |x: CGFloat, y: CGFloat, w: CGFloat, h: CGFloat| CGRect {
        origin: CGPoint { x, y },
        size: CGSize {
            width: w.max(0.0),
            height: h.max(0.0),
        },
    };

    if background_view != nil {
        () = msg![env; background_view setFrame:bounds];
    }

    // The accessory on the right.
    let accessory_width = if accessory_view != nil {
        let view_bounds: CGRect = msg![env; accessory_view bounds];
        let w = view_bounds.size.width;
        let frame = rect(
            width - w - 10.0,
            ((height - view_bounds.size.height) / 2.0).round(),
            w,
            view_bounds.size.height,
        );
        () = msg![env; accessory_view setFrame:frame];
        () = msg![env; accessory_label setHidden:true];
        w + 10.0
    } else if accessory_type != UITableViewCellAccessoryNone {
        let text = match accessory_type {
            UITableViewCellAccessoryDisclosureIndicator => ">",
            UITableViewCellAccessoryDetailDisclosureButton => ">",
            UITableViewCellAccessoryCheckmark => "√",
            _ => "",
        };
        let text = get_static_str(env, text);
        () = msg![env; accessory_label setText:text];
        let color: id = if accessory_type == UITableViewCellAccessoryDisclosureIndicator {
            msg_class![env; UIColor grayColor]
        } else {
            msg_class![env; UIColor colorWithRed:(0.2 as CGFloat) green:(0.4 as CGFloat) blue:(0.8 as CGFloat) alpha:(1.0 as CGFloat)]
        };
        () = msg![env; accessory_label setTextColor:color];
        () = msg![env; accessory_label setFrame:(rect(width - ACCESSORY_WIDTH, 0.0, ACCESSORY_WIDTH - 8.0, height))];
        () = msg![env; accessory_label setHidden:false];
        ACCESSORY_WIDTH
    } else {
        () = msg![env; accessory_label setHidden:true];
        0.0
    };

    let content_width = width - accessory_width;
    () = msg![env; content_view setFrame:(rect(0.0, 0.0, content_width, height))];

    // The image on the left, at its own size.
    let image: id = msg![env; image_view image];
    let mut x = 10.0;
    if image != nil {
        let size: CGSize = msg![env; image size];
        let size = CGSize {
            width: size.width.min(height),
            height: size.height.min(height),
        };
        let frame = rect(
            x,
            ((height - size.height) / 2.0).round(),
            size.width,
            size.height,
        );
        () = msg![env; image_view setFrame:frame];
        x += size.width + 10.0;
    } else {
        () = msg![env; image_view setFrame:(rect(0.0, 0.0, 0.0, 0.0))];
    }

    let text_width = content_width - x - 10.0;
    let detail_text: id = msg![env; detail_text_label text];
    match style {
        UITableViewCellStyleSubtitle if detail_text != nil => {
            () = msg![env; text_label setFrame:(rect(x, 2.0, text_width, height * 0.55))];
            () = msg![env; detail_text_label setFrame:(rect(x, height * 0.55, text_width, height * 0.4))];
        }
        UITableViewCellStyleValue1 => {
            () = msg![env; text_label setFrame:(rect(x, 0.0, text_width * 0.6, height))];
            () = msg![env; detail_text_label setFrame:(rect(x + text_width * 0.4, 0.0, text_width * 0.6, height))];
        }
        UITableViewCellStyleValue2 => {
            () = msg![env; detail_text_label setFrame:(rect(x, 0.0, text_width * 0.3, height))];
            () = msg![env; text_label setFrame:(rect(x + text_width * 0.33, 0.0, text_width * 0.67, height))];
        }
        _ => {
            () = msg![env; text_label setFrame:(rect(x, 0.0, text_width, height))];
            () = msg![env; detail_text_label setFrame:(rect(0.0, 0.0, 0.0, 0.0))];
        }
    }
}

/// Show whether the cell is selected: a blue background with white text.
fn show_selection(env: &mut Environment, cell: id) {
    let &mut UITableViewCellHostObject {
        text_label,
        detail_text_label,
        selected,
        selection_style,
        selected_text_color,
        selected_background_view,
        ..
    } = host(env, cell);
    if text_label == nil {
        return;
    }
    let highlighted = selected && selection_style != UITableViewCellSelectionStyleNone;

    if selected_background_view != nil {
        if highlighted {
            let bounds: CGRect = msg![env; cell bounds];
            () = msg![env; selected_background_view setFrame:bounds];
            () = msg![env; cell insertSubview:selected_background_view atIndex:0i32];
        } else {
            () = msg![env; selected_background_view removeFromSuperview];
        }
        return;
    }

    let background: id = if !highlighted {
        msg_class![env; UIColor whiteColor]
    } else if selection_style == 2 {
        msg_class![env; UIColor lightGrayColor]
    } else {
        msg_class![env; UIColor colorWithRed:(0.02 as CGFloat) green:(0.45 as CGFloat) blue:(0.91 as CGFloat) alpha:(1.0 as CGFloat)]
    };
    () = msg![env; cell setBackgroundColor:background];

    // Labels have no highlighted colour of their own here, so set it.
    if highlighted {
        let color: id = if selected_text_color != nil {
            selected_text_color
        } else {
            msg_class![env; UIColor whiteColor]
        };
        () = msg![env; text_label setTextColor:color];
        () = msg![env; detail_text_label setTextColor:color];
    } else {
        let black: id = msg_class![env; UIColor blackColor];
        () = msg![env; text_label setTextColor:black];
        let gray: id = msg_class![env; UIColor grayColor];
        () = msg![env; detail_text_label setTextColor:gray];
    }
}
