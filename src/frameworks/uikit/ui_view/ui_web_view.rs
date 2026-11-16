/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `UIWebView`.
//!
//! This is not a browser. It shows simple HTML pages, like the chapters of
//! e-book apps: paragraphs and headings become wrapped text, images are
//! shown, and links (text or images) can be tapped, which goes through the
//! delegate like on iPhone OS. Inline formatting and most CSS are ignored.
//! `stringByEvaluatingJavaScriptFromString:` understands a few common
//! snippets (font size, colours, scrolling, the page height).

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
    objc_classes, release, retain, ClassExports, NSZonePtr, SEL,
};
use crate::Environment;

const MARGIN: CGFloat = 10.0;
const BLOCK_GAP: CGFloat = 8.0;
const DEFAULT_FONT_SIZE: CGFloat = 16.0;

/// A piece of a page, laid out one below the other.
enum Block {
    Text {
        text: String,
        /// Relative to the page's font size (headings are bigger).
        scale: CGFloat,
        bold: bool,
        centered: bool,
        href: Option<String>,
    },
    /// Images side by side, each possibly a link.
    Images(Vec<(String, Option<String>)>),
}

struct UIWebViewHostObject {
    superclass: super::UIViewHostObject,
    /// Weak.
    delegate: id,
    /// The scroll view holding the page (retained, a subview).
    scroll_view: id,
    /// Views showing the page (retained, subviews of the scroll view), with
    /// the link each one opens, if any.
    views: Vec<(id, Option<String>)>,
    blocks: Vec<Block>,
    /// Directory relative links and images are found in.
    base_path: Option<String>,
    font_size: CGFloat,
    /// `UIColor*`s set through JavaScript.
    text_color: id,
    background_color: id,
    scales_page_to_fit: bool,
}
impl_HostObject_with_superclass!(UIWebViewHostObject);
impl Default for UIWebViewHostObject {
    fn default() -> Self {
        UIWebViewHostObject {
            superclass: Default::default(),
            delegate: nil,
            scroll_view: nil,
            views: Vec::new(),
            blocks: Vec::new(),
            base_path: None,
            font_size: DEFAULT_FONT_SIZE,
            text_color: nil,
            background_color: nil,
            scales_page_to_fit: false,
        }
    }
}

fn host(env: &mut Environment, this: id) -> &mut UIWebViewHostObject {
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

@implementation UIWebView: UIView

+ (id)allocWithZone:(NSZonePtr)_zone {
    let host_object = Box::<UIWebViewHostObject>::default();
    env.objc.alloc_object(this, host_object, &mut env.mem)
}

- (id)initWithFrame:(CGRect)frame {
    let this: id = msg_super![env; this initWithFrame:frame];
    set_up(env, this);
    this
}

// NSCoding implementation
- (id)initWithCoder:(id)coder {
    let this: id = msg_super![env; this initWithCoder:coder];
    set_up(env, this);
    let key = get_static_str(env, "UIScalesPageToFit");
    let scales: bool = msg![env; coder decodeBoolForKey:key];
    host(env, this).scales_page_to_fit = scales;
    this
}

- (())dealloc {
    clear_views(env, this);
    let host = host(env, this);
    let objects = [host.scroll_view, host.text_color, host.background_color];
    for object in objects {
        release(env, object);
    }
    msg_super![env; this dealloc]
}

- (id)delegate {
    host(env, this).delegate
}
- (())setDelegate:(id)delegate {
    host(env, this).delegate = delegate;
}
- (bool)scalesPageToFit {
    host(env, this).scales_page_to_fit
}
- (())setScalesPageToFit:(bool)scales {
    host(env, this).scales_page_to_fit = scales;
}
- (())setDetectsPhoneNumbers:(bool)_detects {}
- (())setDataDetectorTypes:(NSUInteger)_types {}
- (bool)isLoading {
    false
}
- (bool)canGoBack {
    false
}
- (bool)canGoForward {
    false
}
- (())goBack {}
- (())goForward {}
- (())reload {}
- (())stopLoading {}
- (id)scrollView {
    host(env, this).scroll_view
}

- (())loadHTMLString:(id)html // NSString*
             baseURL:(id)base_url { // NSURL*
    let html = if html == nil { String::new() } else { to_rust_string(env, html).to_string() };
    load(env, this, &html, base_url);
}

- (())loadData:(id)data // NSData*
      MIMEType:(id)_mime_type // NSString*
textEncodingName:(id)_encoding // NSString*
       baseURL:(id)base_url { // NSURL*
    let html: id = msg_class![env; NSString alloc];
    let html: id = msg![env; html initWithData:data encoding:4u32]; // UTF-8
    let text = if html == nil { String::new() } else { to_rust_string(env, html).to_string() };
    release(env, html);
    load(env, this, &text, base_url);
}

- (())loadRequest:(id)request { // NSURLRequest*
    let url: id = if request == nil { nil } else { msg![env; request URL] };
    if url == nil {
        return;
    }
    let is_file: bool = msg![env; url isFileURL];
    if !is_file {
        let description: id = msg![env; url description];
        log!("TODO: UIWebView can't load {} (only files)", to_rust_string(env, description));
        return;
    }
    let path: id = msg![env; url path];
    let html: id = msg_class![env; NSString stringWithContentsOfFile:path];
    let text = if html == nil { String::new() } else { to_rust_string(env, html).to_string() };
    let directory: id = msg![env; path stringByDeletingLastPathComponent];
    let base_url: id = msg_class![env; NSURL fileURLWithPath:directory];
    load(env, this, &text, base_url);
}

- (id)stringByEvaluatingJavaScriptFromString:(id)script { // NSString*
    let script = if script == nil { String::new() } else { to_rust_string(env, script).to_string() };
    let result = evaluate_script(env, this, &script);
    let result = from_rust_string(env, result);
    autorelease(env, result)
}

- (())setFrame:(CGRect)frame {
    () = msg_super![env; this setFrame:frame];
    lay_out(env, this);
}
- (())setBackgroundColor:(id)color {
    () = msg_super![env; this setBackgroundColor:color];
    let scroll_view = host(env, this).scroll_view;
    if scroll_view != nil {
        () = msg![env; scroll_view setBackgroundColor:color];
    }
}

- (())_touchHLE_finishLoad {
    let delegate = host(env, this).delegate;
    if let Some(sel) = responds(env, delegate, "webViewDidFinishLoad:") {
        () = msg_send(env, (delegate, sel, this));
    }
}

- (())_touchHLE_linkTapped:(id)sender {
    let href = host(env, this)
        .views
        .iter()
        .find(|(view, _)| *view == sender)
        .and_then(|(_, href)| href.clone());
    if let Some(href) = href {
        open_link(env, this, &href);
    }
}

@end

};

fn set_up(env: &mut Environment, this: id) {
    let bounds: CGRect = msg![env; this bounds];
    let scroll_view: id = msg_class![env; UIScrollView alloc];
    let scroll_view: id = msg![env; scroll_view initWithFrame:bounds];
    let white: id = msg_class![env; UIColor whiteColor];
    () = msg![env; scroll_view setBackgroundColor:white];
    () = msg![env; this addSubview:scroll_view];
    host(env, this).scroll_view = scroll_view;
}

fn clear_views(env: &mut Environment, this: id) {
    let views = std::mem::take(&mut host(env, this).views);
    for (view, _) in views {
        () = msg![env; view removeFromSuperview];
        release(env, view);
    }
}

fn load(env: &mut Environment, this: id, html: &str, base_url: id) {
    let base_path = if base_url != nil {
        let is_file: bool = msg![env; base_url isFileURL];
        if is_file {
            let path: id = msg![env; base_url path];
            Some(to_rust_string(env, path).to_string())
        } else {
            None
        }
    } else {
        None
    };
    let delegate = host(env, this).delegate;
    if let Some(sel) = responds(env, delegate, "webViewDidStartLoad:") {
        () = msg_send(env, (delegate, sel, this));
    }
    let blocks = parse_html(html);
    let host_object = host(env, this);
    host_object.base_path = base_path;
    host_object.blocks = blocks;
    lay_out(env, this);
    let scroll_view = host(env, this).scroll_view;
    () = msg![env; scroll_view setContentOffset:(CGPoint { x: 0.0, y: 0.0 })];

    // Pages finish loading later, never during the call that loads them.
    let sel = env.objc.lookup_selector("_touchHLE_finishLoad").unwrap();
    () = msg![env; this performSelector:sel withObject:nil afterDelay:(0.0f64)];
}

/// Tell the delegate about a tapped link, and follow it if allowed (only
/// links to other files can be followed).
fn open_link(env: &mut Environment, this: id, href: &str) {
    let base_path = host(env, this).base_path.clone();
    let has_scheme = href
        .split_once(':')
        .is_some_and(|(scheme, _)| !scheme.contains('/'));
    let url: id = if has_scheme || href.starts_with('/') || base_path.is_none() {
        let text = from_rust_string(env, href.to_string());
        let url: id = if href.starts_with('/') {
            msg_class![env; NSURL fileURLWithPath:text]
        } else {
            msg_class![env; NSURL URLWithString:text]
        };
        release(env, text);
        url
    } else {
        let path = format!(
            "{}/{}",
            base_path.unwrap(),
            href.split('#').next().unwrap_or(href)
        );
        let path = from_rust_string(env, path);
        let url: id = msg_class![env; NSURL fileURLWithPath:path];
        release(env, path);
        url
    };
    if url == nil {
        return;
    }
    let request: id = msg_class![env; NSURLRequest requestWithURL:url];
    let delegate = host(env, this).delegate;
    let allowed: bool = if let Some(sel) = responds(
        env,
        delegate,
        "webView:shouldStartLoadWithRequest:navigationType:",
    ) {
        msg_send(env, (delegate, sel, this, request, 0 as NSInteger)) // UIWebViewNavigationTypeLinkClicked
    } else {
        true
    };
    if allowed {
        () = msg![env; this loadRequest:request];
    }
}

/// Lay the page out again, e.g. after loading it or a change of font size.
fn lay_out(env: &mut Environment, this: id) {
    let scroll_view = host(env, this).scroll_view;
    if scroll_view == nil {
        return;
    }
    clear_views(env, this);
    let bounds: CGRect = msg![env; this bounds];
    () = msg![env; scroll_view setFrame:bounds];
    let width = bounds.size.width;
    let text_width = (width - 2.0 * MARGIN).max(10.0);

    let font_size = host(env, this).font_size;
    let text_color = host(env, this).text_color;
    let text_color: id = if text_color != nil {
        text_color
    } else {
        msg_class![env; UIColor blackColor]
    };
    let clear: id = msg_class![env; UIColor clearColor];
    let background = host(env, this).background_color;
    if background != nil {
        () = msg![env; scroll_view setBackgroundColor:background];
    }
    let base_path = host(env, this).base_path.clone();
    let link_action = env.objc.lookup_selector("_touchHLE_linkTapped:").unwrap();

    // Take the blocks out while laying them out, to call methods freely.
    let blocks = std::mem::take(&mut host(env, this).blocks);
    let mut y = MARGIN;
    for block in &blocks {
        match block {
            Block::Text {
                text,
                scale,
                bold,
                centered,
                href,
            } => {
                let size = (font_size * scale).round();
                let font: id = if *bold {
                    msg_class![env; UIFont boldSystemFontOfSize:size]
                } else {
                    msg_class![env; UIFont systemFontOfSize:size]
                };
                let ns_text = from_rust_string(env, text.clone());
                let limit = CGSize {
                    width: text_width,
                    height: 100000.0,
                };
                let measured: CGSize =
                    msg![env; ns_text sizeWithFont:font constrainedToSize:limit lineBreakMode:0i32];
                let frame = CGRect {
                    origin: CGPoint { x: MARGIN, y },
                    size: CGSize {
                        width: text_width,
                        height: measured.height.ceil() + 2.0,
                    },
                };
                let label: id = msg_class![env; UILabel alloc];
                let label: id = msg![env; label initWithFrame:frame];
                () = msg![env; label setNumberOfLines:0i32];
                () = msg![env; label setLineBreakMode:0i32];
                () = msg![env; label setFont:font];
                () = msg![env; label setText:ns_text];
                release(env, ns_text);
                () = msg![env; label setBackgroundColor:clear];
                if href.is_some() {
                    let link_color: id = msg_class![env; UIColor colorWithRed:(0.0 as CGFloat) green:(0.2 as CGFloat) blue:(0.8 as CGFloat) alpha:(1.0 as CGFloat)];
                    () = msg![env; label setTextColor:link_color];
                } else {
                    () = msg![env; label setTextColor:text_color];
                }
                if *centered {
                    () = msg![env; label setTextAlignment:UITextAlignmentCenter];
                }
                () = msg![env; scroll_view addSubview:label];
                host(env, this).views.push((label, None));
                if let Some(href) = href {
                    // An invisible button over the text, for the link.
                    let button: id = msg_class![env; UIButton buttonWithType:UIButtonTypeCustom];
                    retain(env, button);
                    () = msg![env; button setFrame:frame];
                    () = msg![env; button addTarget:this action:link_action forControlEvents:UIControlEventTouchUpInside];
                    () = msg![env; scroll_view addSubview:button];
                    host(env, this).views.push((button, Some(href.clone())));
                }
                y += frame.size.height + BLOCK_GAP;
            }
            Block::Images(images) => {
                // Side by side, the first on the left and the last on the
                // right (e.g. "previous" and "next" page images).
                let mut row_height: CGFloat = 0.0;
                let count = images.len();
                for (i, (src, href)) in images.iter().enumerate() {
                    let image = load_image(env, base_path.as_deref(), src);
                    if image == nil {
                        continue;
                    }
                    let size: CGSize = msg![env; image size];
                    let scale = (text_width / size.width).min(1.0);
                    let size = CGSize {
                        width: (size.width * scale).round(),
                        height: (size.height * scale).round(),
                    };
                    let x = if count == 1 {
                        ((width - size.width) / 2.0).round()
                    } else if i == count - 1 {
                        width - MARGIN - size.width
                    } else {
                        MARGIN + (i as CGFloat) * (text_width / count as CGFloat)
                    };
                    let frame = CGRect {
                        origin: CGPoint { x, y },
                        size,
                    };
                    let view: id = if href.is_some() {
                        let button: id =
                            msg_class![env; UIButton buttonWithType:UIButtonTypeCustom];
                        retain(env, button);
                        () = msg![env; button setImage:image forState:UIControlStateNormal];
                        () = msg![env; button addTarget:this action:link_action forControlEvents:UIControlEventTouchUpInside];
                        button
                    } else {
                        let image_view: id = msg_class![env; UIImageView alloc];
                        msg![env; image_view initWithImage:image]
                    };
                    () = msg![env; view setFrame:frame];
                    () = msg![env; scroll_view addSubview:view];
                    host(env, this).views.push((view, href.clone()));
                    row_height = row_height.max(size.height);
                }
                if row_height > 0.0 {
                    y += row_height + BLOCK_GAP;
                }
            }
        }
    }
    host(env, this).blocks = blocks;

    let content_size = CGSize {
        width,
        height: (y + MARGIN).max(bounds.size.height),
    };
    () = msg![env; scroll_view setContentSize:content_size];
}

fn load_image(env: &mut Environment, base_path: Option<&str>, src: &str) -> id {
    let src = src.split('?').next().unwrap_or(src);
    if let Some(base_path) = base_path {
        let path = from_rust_string(env, format!("{}/{}", base_path, src));
        let image: id = msg_class![env; UIImage imageWithContentsOfFile:path];
        release(env, path);
        if image != nil {
            return image;
        }
    }
    let name = from_rust_string(env, src.rsplit('/').next().unwrap_or(src).to_string());
    let image: id = msg_class![env; UIImage imageNamed:name];
    release(env, name);
    image
}

/// The few JavaScript snippets apps commonly use with pages like these.
fn evaluate_script(env: &mut Environment, this: id, script: &str) -> String {
    let compact: String = script.chars().filter(|c| !c.is_whitespace()).collect();
    let number_after = |text: &str, key: &str| -> Option<CGFloat> {
        let start = text.find(key)? + key.len();
        let digits: String = text[start..]
            .chars()
            .skip_while(|c| !c.is_ascii_digit() && *c != '-')
            .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == '-')
            .collect();
        digits.parse().ok()
    };
    let quoted_after = |text: &str, key: &str| -> Option<String> {
        let start = text.find(key)? + key.len();
        let rest = &text[start..];
        let open = rest.find(['\'', '"'])?;
        let quote = rest[open..].chars().next()?;
        let rest = &rest[open + 1..];
        Some(rest[..rest.find(quote)?].to_string())
    };
    let scroll_view = host(env, this).scroll_view;

    if let Some(rest) = compact.strip_prefix("window.scrollTo(") {
        let y: CGFloat = rest
            .split(',')
            .nth(1)
            .and_then(|y| y.trim_end_matches([')', ';']).parse().ok())
            .unwrap_or(0.0);
        let content: CGSize = msg![env; scroll_view contentSize];
        let bounds: CGRect = msg![env; scroll_view bounds];
        let y = y.min(content.height - bounds.size.height).max(0.0);
        () = msg![env; scroll_view setContentOffset:(CGPoint { x: 0.0, y })];
        return String::new();
    }
    if compact.contains("fontSize") || compact.contains("font-size") {
        let size =
            number_after(&compact, "fontSize").or_else(|| number_after(&compact, "font-size:"));
        if let Some(size) = size.filter(|&size| size > 4.0 && size < 100.0) {
            host(env, this).font_size = size;
            lay_out(env, this);
        }
        return String::new();
    }
    if compact.contains("backgroundColor") || compact.contains("background-color") {
        let value = quoted_after(script, "backgroundColor")
            .or_else(|| quoted_after(script, "background-color"));
        if let Some(color) = value.and_then(|value| parse_color(env, &value)) {
            retain(env, color);
            let old = std::mem::replace(&mut host(env, this).background_color, color);
            release(env, old);
            lay_out(env, this);
        }
        return String::new();
    }
    if compact.contains(".color") || compact.contains("\"color:") {
        let value = quoted_after(script, ".color").or_else(|| {
            quoted_after(script, "color:")
                .map(|v| v.trim_start_matches("color:").trim().to_string())
        });
        let value = value.map(|v| v.trim_start_matches("color:").trim().to_string());
        if let Some(color) = value.and_then(|value| parse_color(env, &value)) {
            retain(env, color);
            let old = std::mem::replace(&mut host(env, this).text_color, color);
            release(env, old);
            lay_out(env, this);
        }
        return String::new();
    }
    if compact.contains("document.height")
        || compact.contains("scrollHeight")
        || compact.contains("offsetHeight")
    {
        let content: CGSize = msg![env; scroll_view contentSize];
        return format!("{}", content.height.round() as i64);
    }
    if compact.contains("scrollY")
        || compact.contains("pageYOffset")
        || compact.contains("scrollTop")
    {
        let offset: CGPoint = msg![env; scroll_view contentOffset];
        return format!("{}", offset.y.round() as i64);
    }
    if compact.contains("styleSheets.length") {
        return "1".to_string();
    }
    if compact.contains("fontFamily")
        || compact.contains("font-family")
        || compact.contains("addRule")
    {
        return String::new();
    }
    log!("TODO: UIWebView JavaScript not understood: {}", script);
    String::new()
}

fn parse_color(env: &mut Environment, value: &str) -> Option<id> {
    let value = value.trim().to_ascii_lowercase();
    let (r, g, b) = if let Some(hex) = value.strip_prefix('#') {
        let digits: Vec<u8> = hex
            .chars()
            .filter_map(|c| c.to_digit(16).map(|d| d as u8))
            .collect();
        match digits.len() {
            3 => (digits[0] * 17, digits[1] * 17, digits[2] * 17),
            6 => (
                digits[0] * 16 + digits[1],
                digits[2] * 16 + digits[3],
                digits[4] * 16 + digits[5],
            ),
            _ => return None,
        }
    } else if let Some(args) = value.strip_prefix("rgb(") {
        let parts: Vec<u8> = args
            .trim_end_matches(')')
            .split(',')
            .filter_map(|p| p.trim().parse().ok())
            .collect();
        if parts.len() != 3 {
            return None;
        }
        (parts[0], parts[1], parts[2])
    } else {
        match value.as_str() {
            "black" => (0, 0, 0),
            "white" => (255, 255, 255),
            "gray" | "grey" => (128, 128, 128),
            "silver" | "lightgray" | "lightgrey" => (192, 192, 192),
            "darkgray" | "darkgrey" => (64, 64, 64),
            "red" => (255, 0, 0),
            "green" => (0, 128, 0),
            "blue" => (0, 0, 255),
            "yellow" => (255, 255, 0),
            "brown" => (165, 42, 42),
            "beige" => (245, 245, 220),
            "ivory" => (255, 255, 240),
            "wheat" => (245, 222, 179),
            "sepia" => (112, 66, 20),
            "navy" => (0, 0, 128),
            "maroon" => (128, 0, 0),
            _ => return None,
        }
    };
    let f = |v: u8| v as CGFloat / 255.0;
    Some(
        msg_class![env; UIColor colorWithRed:(f(r)) green:(f(g)) blue:(f(b)) alpha:(1.0 as CGFloat)],
    )
}

/// Turn a page into blocks. This is a forgiving reader of simple HTML, not a
/// real parser: unknown tags are ignored, and their text is kept.
fn parse_html(html: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut text = String::new();
    let mut images: Vec<(String, Option<String>)> = Vec::new();
    let mut heading: Option<u8> = None;
    let mut centered = 0u32;
    let mut skip_depth = 0u32; // inside <head>, <style>, <script>, <title>
    let mut link: Option<String> = None;
    let mut text_link: Option<String> = None;

    fn flush_text(
        blocks: &mut Vec<Block>,
        text: &mut String,
        heading: Option<u8>,
        centered: bool,
        text_link: &mut Option<String>,
    ) {
        let collapsed = text
            .split('\n')
            .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
            .collect::<Vec<_>>()
            .join("\n");
        let trimmed = collapsed
            .trim_matches(|c: char| c == ' ' || c == '\n')
            .to_string();
        if !trimmed.is_empty() {
            let scale = match heading {
                Some(1) => 1.5,
                Some(2) => 1.3,
                Some(3) => 1.15,
                Some(_) => 1.05,
                None => 1.0,
            };
            blocks.push(Block::Text {
                text: trimmed,
                scale,
                bold: heading.is_some(),
                centered,
                href: text_link.take(),
            });
        }
        text.clear();
        *text_link = None;
    }
    fn flush_images(blocks: &mut Vec<Block>, images: &mut Vec<(String, Option<String>)>) {
        if !images.is_empty() {
            blocks.push(Block::Images(std::mem::take(images)));
        }
    }
    fn attribute(tag: &str, name: &str) -> Option<String> {
        let lower = tag.to_ascii_lowercase();
        let mut search = 0;
        while let Some(found) = lower[search..].find(name) {
            let at = search + found;
            search = at + name.len();
            let before_ok = at == 0 || !lower.as_bytes()[at - 1].is_ascii_alphanumeric();
            let rest = tag[at + name.len()..].trim_start();
            if !before_ok || !rest.starts_with('=') {
                continue;
            }
            let rest = rest[1..].trim_start();
            let value = match rest.chars().next() {
                Some(q @ ('"' | '\'')) => rest[1..].split(q).next().unwrap_or(""),
                _ => rest
                    .split(|c: char| c.is_whitespace() || c == '>' || c == '/')
                    .next()
                    .unwrap_or(""),
            };
            return Some(decode_entities(value));
        }
        None
    }

    let mut rest = html;
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix("<!--") {
            rest = after.find("-->").map_or("", |end| &after[end + 3..]);
            continue;
        }
        if rest.starts_with('<') {
            let Some(end) = rest.find('>') else { break };
            let tag = &rest[1..end];
            rest = &rest[end + 1..];
            let closing = tag.starts_with('/');
            let name: String = tag
                .trim_start_matches('/')
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric())
                .collect::<String>()
                .to_ascii_lowercase();
            let self_closing = tag.ends_with('/');
            match name.as_str() {
                "head" | "style" | "script" | "title" => {
                    if closing {
                        skip_depth = skip_depth.saturating_sub(1);
                    } else if !self_closing {
                        skip_depth += 1;
                    }
                }
                _ if skip_depth > 0 => {}
                "br" => text.push('\n'),
                "img" => {
                    flush_text(
                        &mut blocks,
                        &mut text,
                        heading,
                        centered > 0,
                        &mut text_link,
                    );
                    if let Some(src) = attribute(tag, "src") {
                        images.push((src, link.clone()));
                    }
                }
                "a" => {
                    if closing {
                        link = None;
                    } else {
                        link = attribute(tag, "href");
                    }
                }
                "center" => {
                    flush_text(
                        &mut blocks,
                        &mut text,
                        heading,
                        centered > 0,
                        &mut text_link,
                    );
                    if closing {
                        centered = centered.saturating_sub(1);
                    } else {
                        centered += 1;
                    }
                }
                "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                    flush_images(&mut blocks, &mut images);
                    flush_text(
                        &mut blocks,
                        &mut text,
                        heading,
                        centered > 0,
                        &mut text_link,
                    );
                    heading = if closing {
                        None
                    } else {
                        name[1..].parse().ok()
                    };
                }
                "p" | "div" | "li" | "tr" | "table" | "ul" | "ol" | "blockquote" | "hr"
                | "body" => {
                    // Images side by side stay together across their divs
                    // until some text comes.
                    flush_text(
                        &mut blocks,
                        &mut text,
                        heading,
                        centered > 0,
                        &mut text_link,
                    );
                    let align = attribute(tag, "align")
                        .unwrap_or_default()
                        .to_ascii_lowercase();
                    let style = attribute(tag, "style")
                        .unwrap_or_default()
                        .to_ascii_lowercase()
                        .replace(' ', "");
                    if !closing && (align == "center" || style.contains("text-align:center")) {
                        centered += 1;
                    } else if closing && centered > 0 && name == "div" {
                        // Approximation: a closing div ends centring.
                        centered -= 1;
                    }
                }
                _ => {}
            }
            continue;
        }
        let next = rest.find('<').unwrap_or(rest.len());
        let piece = &rest[..next];
        rest = &rest[next..];
        if skip_depth > 0 {
            continue;
        }
        let decoded = decode_entities(piece);
        if !decoded.trim().is_empty() {
            flush_images(&mut blocks, &mut images);
            if link.is_some() && text_link.is_none() {
                text_link = link.clone();
            }
        }
        text.push_str(&decoded);
    }
    flush_images(&mut blocks, &mut images);
    flush_text(
        &mut blocks,
        &mut text,
        heading,
        centered > 0,
        &mut text_link,
    );
    blocks
}

fn decode_entities(text: &str) -> String {
    if !text.contains('&') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let Some(end) = rest[..rest.len().min(12)].find(';') else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let entity = &rest[1..end];
        let decoded: Option<char> = if let Some(number) = entity.strip_prefix('#') {
            let value = if let Some(hex) = number.strip_prefix(['x', 'X']) {
                u32::from_str_radix(hex, 16).ok()
            } else {
                number.parse().ok()
            };
            value.and_then(char::from_u32)
        } else {
            Some(match entity {
                "amp" => '&',
                "lt" => '<',
                "gt" => '>',
                "quot" => '"',
                "apos" => '\'',
                "nbsp" => '\u{a0}',
                "auml" => 'ä',
                "ouml" => 'ö',
                "uuml" => 'ü',
                "Auml" => 'Ä',
                "Ouml" => 'Ö',
                "Uuml" => 'Ü',
                "szlig" => 'ß',
                "eacute" => 'é',
                "egrave" => 'è',
                "agrave" => 'à',
                "ndash" => '–',
                "mdash" => '—',
                "hellip" => '…',
                "lsquo" => '‘',
                "rsquo" => '’',
                "sbquo" => '‚',
                "ldquo" => '“',
                "rdquo" => '”',
                "bdquo" => '„',
                "laquo" => '«',
                "raquo" => '»',
                "copy" => '©',
                "reg" => '®',
                "shy" => '\u{ad}',
                _ => {
                    out.push('&');
                    rest = &rest[1..];
                    continue;
                }
            })
        };
        match decoded {
            Some(c) if c != '\u{ad}' => out.push(c),
            Some(_) => {}
            None => out.push_str(&rest[..=end]),
        }
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    out
}
