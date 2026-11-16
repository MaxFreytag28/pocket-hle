/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `UITableView`.
//!
//! A simple version: all rows are created when the data is (re)loaded and
//! stacked in the scroll view, with separator lines and section headers and
//! footers. Grouped tables are drawn as inset blocks without rounded
//! corners. Tapping a row selects it; editing isn't supported.

use crate::frameworks::core_graphics::{CGFloat, CGPoint, CGRect, CGSize};
use crate::frameworks::foundation::ns_index_path::{index_path, section_and_row};
use crate::frameworks::foundation::ns_string::get_static_str;
use crate::frameworks::foundation::{NSInteger, NSUInteger};
use crate::frameworks::uikit::ui_view::ui_table_view_cell::{
    accessory_type, is_detail_disclosure, reuse_identifier, ACCESSORY_WIDTH,
};
use crate::objc::{
    autorelease, id, impl_HostObject_with_superclass, msg, msg_class, msg_send, msg_super, nil,
    objc_classes, release, retain, ClassExports, NSZonePtr, SEL,
};
use crate::Environment;

type UITableViewStyle = NSInteger;
const UITableViewStyleGrouped: UITableViewStyle = 1;

type UITableViewCellSeparatorStyle = NSInteger;
const UITableViewCellSeparatorStyleNone: UITableViewCellSeparatorStyle = 0;

/// A row as currently shown.
struct Row {
    section: NSUInteger,
    row: NSUInteger,
    /// `UITableViewCell*`, retained.
    cell: id,
    frame: CGRect,
}

pub struct UITableViewHostObject {
    superclass: super::UIScrollViewHostObject,
    /// Weak.
    data_source: id,
    style: UITableViewStyle,
    row_height: CGFloat,
    section_header_height: CGFloat,
    section_footer_height: CGFloat,
    separator_style: UITableViewCellSeparatorStyle,
    /// `UIColor*`
    separator_color: id,
    /// `UIView*`s
    header_view: id,
    footer_view: id,
    allows_selection: bool,
    loaded: bool,
    rows: Vec<Row>,
    /// Headers, footers and separators shown (retained).
    other_views: Vec<id>,
    /// Cells that can be reused (retained).
    reusable_cells: Vec<id>,
    selected: Option<(NSUInteger, NSUInteger)>,
    /// Where the current touch started, and whether it has moved (scrolled)
    /// since.
    touch_start: Option<CGPoint>,
    touch_moved: bool,
}
impl_HostObject_with_superclass!(UITableViewHostObject);
impl Default for UITableViewHostObject {
    fn default() -> Self {
        UITableViewHostObject {
            superclass: Default::default(),
            data_source: nil,
            style: 0,
            row_height: 44.0,
            section_header_height: 22.0,
            section_footer_height: 22.0,
            separator_style: 1,
            separator_color: nil,
            header_view: nil,
            footer_view: nil,
            allows_selection: true,
            loaded: false,
            rows: Vec::new(),
            other_views: Vec::new(),
            reusable_cells: Vec::new(),
            selected: None,
            touch_start: None,
            touch_moved: false,
        }
    }
}

fn host(env: &mut Environment, table: id) -> &mut UITableViewHostObject {
    env.objc.borrow_mut(table)
}

/// The selector, if `object` responds to it.
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

@implementation UITableView: UIScrollView

+ (id)allocWithZone:(NSZonePtr)_zone {
    let host_object = Box::<UITableViewHostObject>::default();
    env.objc.alloc_object(this, host_object, &mut env.mem)
}

- (id)initWithFrame:(CGRect)frame style:(UITableViewStyle)style {
    let this: id = msg_super![env; this initWithFrame:frame];
    set_style(env, this, style);
    this
}
- (id)initWithFrame:(CGRect)frame {
    msg![env; this initWithFrame:frame style:0]
}

- (id)initWithCoder:(id)coder {
    let this: id = msg_super![env; this initWithCoder:coder];
    let key = get_static_str(env, "UIStyle");
    let style: NSInteger = msg![env; coder decodeIntegerForKey:key];
    set_style(env, this, style);
    for (key, field) in [
        ("UIRowHeight", 0),
        ("UISectionHeaderHeight", 1),
        ("UISectionFooterHeight", 2),
    ] {
        let key = get_static_str(env, key);
        let present: bool = msg![env; coder containsValueForKey:key];
        if present {
            let value: f32 = msg![env; coder decodeFloatForKey:key];
            let host = host(env, this);
            match field {
                0 => host.row_height = value,
                1 => host.section_header_height = value,
                _ => host.section_footer_height = value,
            }
        }
    }
    let key = get_static_str(env, "UISeparatorStyle");
    let present: bool = msg![env; coder containsValueForKey:key];
    if present {
        let value: NSInteger = msg![env; coder decodeIntegerForKey:key];
        host(env, this).separator_style = value;
    }
    let key = get_static_str(env, "UIAllowsSelection");
    let present: bool = msg![env; coder containsValueForKey:key];
    if present {
        let value: bool = msg![env; coder decodeBoolForKey:key];
        host(env, this).allows_selection = value;
    }
    this
}

- (())dealloc {
    clear_rows(env, this);
    let host = host(env, this);
    let reusable = std::mem::take(&mut host.reusable_cells);
    let objects = [host.separator_color, host.header_view, host.footer_view];
    for object in reusable.into_iter().chain(objects) {
        release(env, object);
    }
    msg_super![env; this dealloc]
}

- (id)dataSource {
    host(env, this).data_source
}
- (())setDataSource:(id)data_source {
    host(env, this).data_source = data_source;
    host(env, this).loaded = false;
    () = msg![env; this setNeedsDisplay];
}
- (UITableViewStyle)style {
    host(env, this).style
}
- (CGFloat)rowHeight {
    host(env, this).row_height
}
- (())setRowHeight:(CGFloat)height {
    host(env, this).row_height = height;
}
- (CGFloat)sectionHeaderHeight {
    host(env, this).section_header_height
}
- (())setSectionHeaderHeight:(CGFloat)height {
    host(env, this).section_header_height = height;
}
- (CGFloat)sectionFooterHeight {
    host(env, this).section_footer_height
}
- (())setSectionFooterHeight:(CGFloat)height {
    host(env, this).section_footer_height = height;
}
- (UITableViewCellSeparatorStyle)separatorStyle {
    host(env, this).separator_style
}
- (())setSeparatorStyle:(UITableViewCellSeparatorStyle)style {
    host(env, this).separator_style = style;
}
- (id)separatorColor {
    host(env, this).separator_color
}
- (())setSeparatorColor:(id)color { // UIColor*
    retain(env, color);
    let old = std::mem::replace(&mut host(env, this).separator_color, color);
    release(env, old);
}
- (id)tableHeaderView {
    host(env, this).header_view
}
- (())setTableHeaderView:(id)view { // UIView*
    retain(env, view);
    let old = std::mem::replace(&mut host(env, this).header_view, view);
    if old != nil {
        () = msg![env; old removeFromSuperview];
    }
    release(env, old);
    relayout_if_loaded(env, this);
}
- (id)tableFooterView {
    host(env, this).footer_view
}
- (())setTableFooterView:(id)view { // UIView*
    retain(env, view);
    let old = std::mem::replace(&mut host(env, this).footer_view, view);
    if old != nil {
        () = msg![env; old removeFromSuperview];
    }
    release(env, old);
    relayout_if_loaded(env, this);
}
- (bool)allowsSelection {
    host(env, this).allows_selection
}
- (())setAllowsSelection:(bool)allows {
    host(env, this).allows_selection = allows;
}
- (())setAllowsSelectionDuringEditing:(bool)_allows {}
- (bool)isEditing {
    false
}
- (())setEditing:(bool)_editing {}
- (())setEditing:(bool)_editing animated:(bool)_animated {}
- (())setSectionIndexMinimumDisplayRowCount:(NSInteger)_count {}

- (())reloadData {
    reload(env, this);
}
- (())beginUpdates {}
- (())endUpdates {
    reload(env, this);
}
- (())insertRowsAtIndexPaths:(id)_paths withRowAnimation:(NSInteger)_animation {
    reload(env, this);
}
- (())deleteRowsAtIndexPaths:(id)_paths withRowAnimation:(NSInteger)_animation {
    reload(env, this);
}
- (())reloadRowsAtIndexPaths:(id)_paths withRowAnimation:(NSInteger)_animation {
    reload(env, this);
}
- (())insertSections:(id)_sections withRowAnimation:(NSInteger)_animation {
    reload(env, this);
}
- (())deleteSections:(id)_sections withRowAnimation:(NSInteger)_animation {
    reload(env, this);
}
- (())reloadSections:(id)_sections withRowAnimation:(NSInteger)_animation {
    reload(env, this);
}

- (id)dequeueReusableCellWithIdentifier:(id)identifier { // NSString*
    if identifier == nil {
        return nil;
    }
    let reusable = host(env, this).reusable_cells.clone();
    for (i, cell) in reusable.into_iter().enumerate() {
        let cell_identifier = reuse_identifier(env, cell);
        let same: bool = msg![env; identifier isEqualToString:cell_identifier];
        if same {
            host(env, this).reusable_cells.remove(i);
            () = msg![env; cell prepareForReuse];
            return autorelease(env, cell);
        }
    }
    nil
}

- (NSInteger)numberOfSections {
    ensure_loaded(env, this);
    host(env, this).rows.iter().map(|r| r.section + 1).max().unwrap_or(0) as NSInteger
}
- (NSInteger)numberOfRowsInSection:(NSInteger)section {
    ensure_loaded(env, this);
    host(env, this).rows.iter().filter(|r| r.section == section as NSUInteger).count() as NSInteger
}
- (id)cellForRowAtIndexPath:(id)path { // NSIndexPath*
    ensure_loaded(env, this);
    let (section, row) = section_and_row(env, path);
    host(env, this)
        .rows
        .iter()
        .find(|r| r.section == section && r.row == row)
        .map_or(nil, |r| r.cell)
}
- (id)indexPathForCell:(id)cell { // UITableViewCell*
    let found = host(env, this).rows.iter().find(|r| r.cell == cell).map(|r| (r.section, r.row));
    found.map_or(nil, |(section, row)| index_path(env, vec![section, row]))
}
- (id)indexPathForRowAtPoint:(CGPoint)point {
    let found = row_at(env, this, point);
    found.map_or(nil, |(section, row)| index_path(env, vec![section, row]))
}
- (CGRect)rectForRowAtIndexPath:(id)path { // NSIndexPath*
    let (section, row) = section_and_row(env, path);
    host(env, this)
        .rows
        .iter()
        .find(|r| r.section == section && r.row == row)
        .map_or(CGRect { origin: CGPoint { x: 0.0, y: 0.0 }, size: CGSize { width: 0.0, height: 0.0 } }, |r| r.frame)
}
- (id)visibleCells {
    let cells: Vec<id> = host(env, this).rows.iter().map(|r| r.cell).collect();
    for &cell in &cells {
        retain(env, cell);
    }
    let array = crate::frameworks::foundation::ns_array::from_vec(env, cells);
    autorelease(env, array)
}
- (id)indexPathsForVisibleRows {
    let positions: Vec<_> = host(env, this).rows.iter().map(|r| (r.section, r.row)).collect();
    let paths: Vec<id> = positions
        .into_iter()
        .map(|(section, row)| {
            let path = index_path(env, vec![section, row]);
            retain(env, path)
        })
        .collect();
    let array = crate::frameworks::foundation::ns_array::from_vec(env, paths);
    autorelease(env, array)
}

- (id)indexPathForSelectedRow {
    let selected = host(env, this).selected;
    selected.map_or(nil, |(section, row)| index_path(env, vec![section, row]))
}
- (())selectRowAtIndexPath:(id)path // NSIndexPath*
                  animated:(bool)_animated
            scrollPosition:(NSInteger)position {
    if path == nil {
        set_selected(env, this, None);
        return;
    }
    let position_pair = section_and_row(env, path);
    set_selected(env, this, Some(position_pair));
    if position != 0 {
        () = msg![env; this scrollToRowAtIndexPath:path atScrollPosition:position animated:false];
    }
}
- (())deselectRowAtIndexPath:(id)path // NSIndexPath*
                    animated:(bool)_animated {
    let position = section_and_row(env, path);
    if host(env, this).selected == Some(position) {
        set_selected(env, this, None);
    }
}
- (())scrollToRowAtIndexPath:(id)path // NSIndexPath*
            atScrollPosition:(NSInteger)position
                    animated:(bool)_animated {
    let frame: CGRect = msg![env; this rectForRowAtIndexPath:path];
    let bounds: CGRect = msg![env; this bounds];
    let content_size: CGSize = msg![env; this contentSize];
    let y = match position {
        2 => frame.origin.y + (frame.size.height - bounds.size.height) / 2.0, // middle
        3 => frame.origin.y + frame.size.height - bounds.size.height, // bottom
        _ => frame.origin.y, // none, top
    };
    let max_y = (content_size.height - bounds.size.height).max(0.0);
    let offset = CGPoint { x: 0.0, y: y.min(max_y).max(0.0) };
    () = msg![env; this setContentOffset:offset];
}
- (())scrollToNearestSelectedRowAtScrollPosition:(NSInteger)_position animated:(bool)_animated {}
- (())flashScrollIndicators {}

// The data is loaded the first time the table is drawn, as on iPhone OS.
- (())drawRect:(CGRect)_rect {
    ensure_loaded(env, this);
}
- (())layoutSubviews {
    ensure_loaded(env, this);
}

- (())touchesBegan:(id)touches withEvent:(id)_event { // NSSet* of UITouch*
    let touch: id = msg![env; touches anyObject];
    let location: CGPoint = msg![env; touch locationInView:this];
    let host = host(env, this);
    host.touch_start = Some(location);
    host.touch_moved = false;
}
- (())touchesMoved:(id)touches withEvent:(id)event { // NSSet* of UITouch*
    let touch: id = msg![env; touches anyObject];
    let location: CGPoint = msg![env; touch locationInView:this];
    if let Some(start) = host(env, this).touch_start {
        if (location.x - start.x).abs() > 8.0 || (location.y - start.y).abs() > 8.0 {
            host(env, this).touch_moved = true;
        }
    }
    () = msg_super![env; this touchesMoved:touches withEvent:event];
}
- (())touchesEnded:(id)touches withEvent:(id)_event { // NSSet* of UITouch*
    let touch: id = msg![env; touches anyObject];
    let location: CGPoint = msg![env; touch locationInView:this];
    let host = host(env, this);
    let tapped = host.touch_start.is_some() && !host.touch_moved;
    host.touch_start = None;
    if tapped {
        row_tapped(env, this, location);
    }
}

@end

};

fn set_style(env: &mut Environment, table: id, style: UITableViewStyle) {
    let host = host(env, table);
    host.style = style;
    if style == UITableViewStyleGrouped {
        host.section_header_height = 10.0;
        host.section_footer_height = 10.0;
    }
    let background: id = if style == UITableViewStyleGrouped {
        msg_class![env; UIColor colorWithRed:(0.77 as CGFloat) green:(0.8 as CGFloat) blue:(0.84 as CGFloat) alpha:(1.0 as CGFloat)]
    } else {
        msg_class![env; UIColor whiteColor]
    };
    () = msg![env; table setBackgroundColor:background];
    () = msg![env; table setNeedsDisplay];
}

fn ensure_loaded(env: &mut Environment, table: id) {
    if !host(env, table).loaded {
        reload(env, table);
    }
}

fn relayout_if_loaded(env: &mut Environment, table: id) {
    if host(env, table).loaded {
        reload(env, table);
    }
}

/// Take the current rows and other views off the table. Cells with a reuse
/// identifier are kept for reuse.
fn clear_rows(env: &mut Environment, table: id) {
    let rows = std::mem::take(&mut host(env, table).rows);
    let others = std::mem::take(&mut host(env, table).other_views);
    for Row { cell, .. } in rows {
        () = msg![env; cell removeFromSuperview];
        if reuse_identifier(env, cell) != nil {
            host(env, table).reusable_cells.push(cell);
        } else {
            release(env, cell);
        }
    }
    for view in others {
        () = msg![env; view removeFromSuperview];
        release(env, view);
    }
}

/// A label for a section header or footer.
fn section_label(
    env: &mut Environment,
    title: id,
    frame: CGRect,
    grouped: bool,
    header: bool,
) -> id {
    let label: id = msg_class![env; UILabel alloc];
    let label: id = msg![env; label initWithFrame:frame];
    () = msg![env; label setText:title];
    let size: CGFloat = if header { 17.0 } else { 15.0 };
    let font: id = if header {
        msg_class![env; UIFont boldSystemFontOfSize:size]
    } else {
        msg_class![env; UIFont systemFontOfSize:size]
    };
    () = msg![env; label setFont:font];
    if grouped {
        let clear: id = msg_class![env; UIColor clearColor];
        () = msg![env; label setBackgroundColor:clear];
        let color: id = msg_class![env; UIColor colorWithRed:(0.3 as CGFloat) green:(0.34 as CGFloat) blue:(0.42 as CGFloat) alpha:(1.0 as CGFloat)];
        () = msg![env; label setTextColor:color];
    } else {
        let background: id = msg_class![env; UIColor colorWithRed:(0.6 as CGFloat) green:(0.65 as CGFloat) blue:(0.71 as CGFloat) alpha:(1.0 as CGFloat)];
        () = msg![env; label setBackgroundColor:background];
        let white: id = msg_class![env; UIColor whiteColor];
        () = msg![env; label setTextColor:white];
    }
    label
}

fn add_other_view(env: &mut Environment, table: id, view: id, frame: CGRect) {
    () = msg![env; view setFrame:frame];
    () = msg![env; table addSubview:view];
    retain(env, view);
    host(env, table).other_views.push(view);
}

/// Ask the data source for everything again and lay it out.
fn reload(env: &mut Environment, table: id) {
    clear_rows(env, table);
    host(env, table).loaded = true;

    let &mut UITableViewHostObject {
        data_source,
        style,
        row_height,
        section_header_height,
        section_footer_height,
        separator_style,
        separator_color,
        header_view,
        footer_view,
        ..
    } = host(env, table);
    let delegate: id = msg![env; table delegate];
    let grouped = style == UITableViewStyleGrouped;
    let bounds: CGRect = msg![env; table bounds];
    let width = bounds.size.width;
    let inset: CGFloat = if grouped { 10.0 } else { 0.0 };
    let rect = |x: CGFloat, y: CGFloat, w: CGFloat, h: CGFloat| CGRect {
        origin: CGPoint { x, y },
        size: CGSize {
            width: w,
            height: h,
        },
    };

    let mut y: CGFloat = 0.0;
    if header_view != nil {
        let header_bounds: CGRect = msg![env; header_view bounds];
        let frame = rect(0.0, 0.0, width, header_bounds.size.height);
        () = msg![env; header_view setFrame:frame];
        () = msg![env; table addSubview:header_view];
        y += header_bounds.size.height;
    }

    let sections: NSInteger = if data_source == nil {
        0
    } else if let Some(sel) = responds(env, data_source, "numberOfSectionsInTableView:") {
        msg_send(env, (data_source, sel, table))
    } else {
        1
    };
    let rows_sel = responds(env, data_source, "tableView:numberOfRowsInSection:");
    let cell_sel = responds(env, data_source, "tableView:cellForRowAtIndexPath:");
    let header_title_sel = responds(env, data_source, "tableView:titleForHeaderInSection:");
    let footer_title_sel = responds(env, data_source, "tableView:titleForFooterInSection:");
    let header_view_sel = responds(env, delegate, "tableView:viewForHeaderInSection:");
    let header_height_sel = responds(env, delegate, "tableView:heightForHeaderInSection:");
    let footer_view_sel = responds(env, delegate, "tableView:viewForFooterInSection:");
    let footer_height_sel = responds(env, delegate, "tableView:heightForFooterInSection:");
    let row_height_sel = responds(env, delegate, "tableView:heightForRowAtIndexPath:");
    let will_display_sel = responds(
        env,
        delegate,
        "tableView:willDisplayCell:forRowAtIndexPath:",
    );

    for section in 0..sections.max(0) {
        let rows: NSInteger =
            rows_sel.map_or(0, |sel| msg_send(env, (data_source, sel, table, section)));

        // Header: a view from the delegate, or a title from the data source.
        let header_custom: id =
            header_view_sel.map_or(nil, |sel| msg_send(env, (delegate, sel, table, section)));
        let header_title: id =
            header_title_sel.map_or(nil, |sel| msg_send(env, (data_source, sel, table, section)));
        let default_header = if header_custom != nil || header_title != nil {
            if grouped {
                36.0
            } else {
                section_header_height
            }
        } else if grouped {
            section_header_height
        } else {
            0.0
        };
        let header_height: CGFloat = header_height_sel.map_or(default_header, |sel| {
            msg_send(env, (delegate, sel, table, section))
        });
        if header_custom != nil {
            add_other_view(
                env,
                table,
                header_custom,
                rect(0.0, y, width, header_height),
            );
        } else if header_title != nil {
            let label_inset = if grouped { 20.0 } else { 10.0 };
            let label = section_label(
                env,
                header_title,
                rect(0.0, y, width, header_height),
                grouped,
                true,
            );
            // Plain headers span the width with the text inset; grouped
            // headers are text only.
            if grouped {
                let frame = rect(label_inset, y, width - 2.0 * label_inset, header_height);
                add_other_view(env, table, label, frame);
            } else {
                add_other_view(env, table, label, rect(0.0, y, width, header_height));
                let title: id = msg![env; label text];
                let padded = crate::frameworks::foundation::ns_string::to_rust_string(env, title)
                    .to_string();
                let padded = crate::frameworks::foundation::ns_string::from_rust_string(
                    env,
                    format!("  {}", padded),
                );
                () = msg![env; label setText:padded];
                release(env, padded);
            }
            release(env, label);
        }
        y += header_height;

        for row in 0..rows.max(0) {
            let path = index_path(env, vec![section as NSUInteger, row as NSUInteger]);
            let height: CGFloat = row_height_sel.map_or(row_height, |sel| {
                msg_send(env, (delegate, sel, table, path))
            });
            let cell: id =
                cell_sel.map_or(nil, |sel| msg_send(env, (data_source, sel, table, path)));
            if cell == nil {
                y += height;
                continue;
            }
            retain(env, cell);
            let frame = rect(inset, y, width - 2.0 * inset, height);
            () = msg![env; cell setFrame:frame];
            () = msg![env; table addSubview:cell];
            if let Some(sel) = will_display_sel {
                () = msg_send(env, (delegate, sel, table, cell, path));
            }
            host(env, table).rows.push(Row {
                section: section as NSUInteger,
                row: row as NSUInteger,
                cell,
                frame,
            });

            let last_in_grouped_section = grouped && row == rows - 1;
            if separator_style != UITableViewCellSeparatorStyleNone && !last_in_grouped_section {
                let line: id = msg_class![env; UIView alloc];
                let line_frame = rect(inset, y + height - 1.0, width - 2.0 * inset, 1.0);
                let line: id = msg![env; line initWithFrame:line_frame];
                let color: id = if separator_color != nil {
                    separator_color
                } else {
                    msg_class![env; UIColor colorWithRed:(0.88 as CGFloat) green:(0.88 as CGFloat) blue:(0.88 as CGFloat) alpha:(1.0 as CGFloat)]
                };
                () = msg![env; line setBackgroundColor:color];
                add_other_view(env, table, line, line_frame);
                release(env, line);
            }
            y += height;
        }

        // Footer.
        let footer_custom: id =
            footer_view_sel.map_or(nil, |sel| msg_send(env, (delegate, sel, table, section)));
        let footer_title: id =
            footer_title_sel.map_or(nil, |sel| msg_send(env, (data_source, sel, table, section)));
        let default_footer = if footer_custom != nil || footer_title != nil {
            section_footer_height.max(30.0)
        } else if grouped {
            section_footer_height
        } else {
            0.0
        };
        let footer_height: CGFloat = footer_height_sel.map_or(default_footer, |sel| {
            msg_send(env, (delegate, sel, table, section))
        });
        if footer_custom != nil {
            add_other_view(
                env,
                table,
                footer_custom,
                rect(0.0, y, width, footer_height),
            );
        } else if footer_title != nil {
            let frame = rect(20.0, y, width - 40.0, footer_height);
            let label = section_label(env, footer_title, frame, true, false);
            add_other_view(env, table, label, frame);
            release(env, label);
        }
        y += footer_height;
    }

    if footer_view != nil {
        let footer_bounds: CGRect = msg![env; footer_view bounds];
        let frame = rect(0.0, y, width, footer_bounds.size.height);
        () = msg![env; footer_view setFrame:frame];
        () = msg![env; table addSubview:footer_view];
        y += footer_bounds.size.height;
    }

    let content_size = CGSize { width, height: y };
    () = msg![env; table setContentSize:content_size];
    let offset: CGPoint = msg![env; table contentOffset];
    let max_y = (y - bounds.size.height).max(0.0);
    if offset.y > max_y {
        let offset = CGPoint {
            x: offset.x,
            y: max_y,
        };
        () = msg![env; table setContentOffset:offset];
    }

    // Keep the selection if that row still exists.
    let selected = host(env, table).selected;
    host(env, table).selected = None;
    if let Some(position) = selected {
        if host(env, table)
            .rows
            .iter()
            .any(|r| (r.section, r.row) == position)
        {
            set_selected(env, table, Some(position));
        }
    }
}

fn row_at(env: &mut Environment, table: id, point: CGPoint) -> Option<(NSUInteger, NSUInteger)> {
    host(env, table)
        .rows
        .iter()
        .find(|r| {
            point.x >= r.frame.origin.x
                && point.x < r.frame.origin.x + r.frame.size.width
                && point.y >= r.frame.origin.y
                && point.y < r.frame.origin.y + r.frame.size.height
        })
        .map(|r| (r.section, r.row))
}

fn cell_at(env: &mut Environment, table: id, position: (NSUInteger, NSUInteger)) -> id {
    host(env, table)
        .rows
        .iter()
        .find(|r| (r.section, r.row) == position)
        .map_or(nil, |r| r.cell)
}

fn set_selected(env: &mut Environment, table: id, position: Option<(NSUInteger, NSUInteger)>) {
    let old = std::mem::replace(&mut host(env, table).selected, position);
    if let Some(old) = old {
        let cell = cell_at(env, table, old);
        if cell != nil {
            () = msg![env; cell setSelected:false];
        }
    }
    if let Some(new) = position {
        let cell = cell_at(env, table, new);
        if cell != nil {
            () = msg![env; cell setSelected:true];
        }
    }
}

/// A tap (not a scroll) ended at `point`: select the row there and tell the
/// delegate, as on iPhone OS.
fn row_tapped(env: &mut Environment, table: id, point: CGPoint) {
    let Some(position) = row_at(env, table, point) else {
        return;
    };
    let delegate: id = msg![env; table delegate];
    let path = index_path(env, vec![position.0, position.1]);

    // The detail disclosure button has its own message.
    let cell = cell_at(env, table, position);
    let frame = host(env, table)
        .rows
        .iter()
        .find(|r| (r.section, r.row) == position)
        .unwrap()
        .frame;
    if cell != nil
        && is_detail_disclosure(accessory_type(env, cell))
        && point.x >= frame.origin.x + frame.size.width - ACCESSORY_WIDTH
    {
        if let Some(sel) = responds(
            env,
            delegate,
            "tableView:accessoryButtonTappedForRowWithIndexPath:",
        ) {
            () = msg_send(env, (delegate, sel, table, path));
            return;
        }
    }

    if !host(env, table).allows_selection {
        return;
    }
    let path = if let Some(sel) = responds(env, delegate, "tableView:willSelectRowAtIndexPath:") {
        let path: id = msg_send(env, (delegate, sel, table, path));
        path
    } else {
        path
    };
    if path == nil {
        return;
    }
    let position = section_and_row(env, path);
    if let Some(old) = host(env, table).selected {
        if old != position {
            if let Some(sel) = responds(env, delegate, "tableView:willDeselectRowAtIndexPath:") {
                let old_path = index_path(env, vec![old.0, old.1]);
                let _: id = msg_send(env, (delegate, sel, table, old_path));
            }
        }
    }
    set_selected(env, table, Some(position));
    if let Some(sel) = responds(env, delegate, "tableView:didSelectRowAtIndexPath:") {
        () = msg_send(env, (delegate, sel, table, path));
    }
}
