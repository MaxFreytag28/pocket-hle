/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */

use crate::objc::{id, nil, objc_classes, ClassExports};

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

@implementation SKPaymentQueue: NSObject

+ (id)defaultQueue {
    // TODO
    nil
}

+ (bool)canMakePayments {
    false
}

@end

// There is no store to buy from, so payments can't be created. Apps are
// expected to check `canMakePayments` first, but not all do.
@implementation SKPayment: NSObject

+ (id)paymentWithProductIdentifier:(id)_identifier { // NSString*
    nil
}

@end

@implementation SKMutablePayment: SKPayment
@end

};
