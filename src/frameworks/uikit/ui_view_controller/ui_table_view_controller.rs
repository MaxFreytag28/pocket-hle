/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `UITableViewController`.

use crate::frameworks::core_graphics::CGRect;
use crate::frameworks::foundation::NSInteger;
use crate::objc::{
    id, impl_HostObject_with_superclass, msg, msg_class, msg_super, nil, objc_classes,
    ClassExports, NSZonePtr,
};

#[derive(Default)]
struct UITableViewControllerHostObject {
    superclass: super::UIViewControllerHostObject,
    style: NSInteger,
}
impl_HostObject_with_superclass!(UITableViewControllerHostObject);

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

@implementation UITableViewController: UIViewController

+ (id)allocWithZone:(NSZonePtr)_zone {
    let host_object = Box::<UITableViewControllerHostObject>::default();
    env.objc.alloc_object(this, host_object, &mut env.mem)
}

- (id)initWithStyle:(NSInteger)style {
    env.objc.borrow_mut::<UITableViewControllerHostObject>(this).style = style;
    this
}

- (())loadView {
    // From a nib file if the controller has one, otherwise a new table view
    // filling the screen.
    let has_nib = env.objc.borrow::<UITableViewControllerHostObject>(this).superclass.nib_name != nil;
    if has_nib {
        () = msg_super![env; this loadView];
    } else {
        let style = env.objc.borrow::<UITableViewControllerHostObject>(this).style;
        let screen: id = msg_class![env; UIScreen mainScreen];
        let frame: CGRect = msg![env; screen applicationFrame];
        let table: id = msg_class![env; UITableView alloc];
        let table: id = msg![env; table initWithFrame:frame style:style];
        () = msg![env; this setView:table];
        crate::objc::release(env, table);
    }
    let table: id = msg![env; this tableView];
    if table != nil {
        let data_source: id = msg![env; table dataSource];
        if data_source == nil {
            () = msg![env; table setDataSource:this];
        }
        let delegate: id = msg![env; table delegate];
        if delegate == nil {
            () = msg![env; table setDelegate:this];
        }
    }
}

- (id)tableView {
    let view: id = msg![env; this view];
    let class = env.objc.get_known_class("UITableView", &mut env.mem);
    let is_table: bool = msg![env; view isKindOfClass:class];
    if is_table { view } else { nil }
}
- (())setTableView:(id)table { // UITableView*
    () = msg![env; this setView:table];
}
- (())setClearsSelectionOnViewWillAppear:(bool)_clears {}

@end

};
