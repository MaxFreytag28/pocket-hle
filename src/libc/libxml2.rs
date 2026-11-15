/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! A small subset of `libxml2.dylib`: tree parsing, tree construction and a
//! subset of XPath.
//!
//! libxml2's tree API is not opaque: apps (and wrappers like TouchXML) read
//! the fields of `xmlNode`, `xmlDoc`, `xmlAttr` etc directly. So the tree is
//! built in guest memory using the same struct layouts as the real library
//! (for a 32-bit target), and the functions here operate on that guest tree.
//!
//! This is an independent implementation, it shares no code with libxml2.
//! Only UTF-8 documents are supported, and DTDs and entity declarations are
//! skipped.

use crate::dyld::{export_c_func, ConstantExports, FunctionExports, HostConstant, HostDylib};
use crate::mem::{ConstPtr, ConstVoidPtr, GuestUSize, MutPtr, MutVoidPtr, Ptr};
use crate::Environment;
use std::collections::HashSet;

// Field offsets shared by xmlNode, xmlAttr and xmlDoc.
const N_TYPE: u32 = 4;
const N_NAME: u32 = 8;
const N_CHILDREN: u32 = 12;
const N_LAST: u32 = 16;
const N_PARENT: u32 = 20;
const N_NEXT: u32 = 24;
const N_PREV: u32 = 28;
const N_DOC: u32 = 32;
// xmlNode and xmlAttr only.
const N_NS: u32 = 36;
// xmlNode only.
const N_CONTENT: u32 = 40;
const N_PROPERTIES: u32 = 44;
const N_NSDEF: u32 = 48;
const SIZEOF_NODE: u32 = 60;
const SIZEOF_ATTR: u32 = 48;
// xmlDoc only.
const D_STANDALONE: u32 = 40;
const D_VERSION: u32 = 56;
const D_ENCODING: u32 = 60;
const SIZEOF_DOC: u32 = 96;
// xmlNs.
const NS_NEXT: u32 = 0;
const NS_TYPE: u32 = 4;
const NS_HREF: u32 = 8;
const NS_PREFIX: u32 = 12;
const SIZEOF_NS: u32 = 24;
// xmlXPathContext. We only use the first two fields, but apps may poke others.
const XC_DOC: u32 = 0;
const XC_NODE: u32 = 4;
const SIZEOF_XPATH_CONTEXT: u32 = 256;
// xmlXPathObject.
const XO_TYPE: u32 = 0;
const XO_NODESETVAL: u32 = 4;
const SIZEOF_XPATH_OBJECT: u32 = 64;
// xmlNodeSet.
const NSET_NODE_NR: u32 = 0;
const NSET_NODE_MAX: u32 = 4;
const NSET_NODE_TAB: u32 = 8;
const SIZEOF_NODE_SET: u32 = 12;

// xmlElementType
const XML_ELEMENT_NODE: u32 = 1;
const XML_ATTRIBUTE_NODE: u32 = 2;
const XML_TEXT_NODE: u32 = 3;
const XML_CDATA_SECTION_NODE: u32 = 4;
const XML_PI_NODE: u32 = 7;
const XML_COMMENT_NODE: u32 = 8;
const XML_DOCUMENT_NODE: u32 = 9;
const XML_NAMESPACE_DECL: u32 = 18;

const XPATH_NODESET: u32 = 1;

#[derive(Default)]
pub struct State {
    /// Shared names for text and comment nodes (these are static strings in
    /// the real library too, and must never be freed).
    text_name: u32,
    comment_name: u32,
}
impl State {
    fn get_mut(env: &mut Environment) -> &mut Self {
        &mut env.libc_state.libxml2
    }
}

// ---------------------------------------------------------------------------
// Guest memory helpers
// ---------------------------------------------------------------------------

fn rd(env: &Environment, addr: u32, off: u32) -> u32 {
    let ptr: ConstPtr<u32> = Ptr::from_bits(addr + off);
    env.mem.read(ptr)
}
fn wr(env: &mut Environment, addr: u32, off: u32, value: u32) {
    let ptr: MutPtr<u32> = Ptr::from_bits(addr + off);
    env.mem.write(ptr, value)
}
fn calloc(env: &mut Environment, size: u32) -> u32 {
    env.mem.calloc(size).to_bits()
}
fn free(env: &mut Environment, addr: u32) {
    if addr != 0 {
        let ptr: MutVoidPtr = Ptr::from_bits(addr);
        env.mem.free(ptr);
    }
}
fn new_cstr(env: &mut Environment, s: &str) -> u32 {
    env.mem.alloc_and_write_cstr(s.as_bytes()).to_bits()
}
fn read_cstr(env: &Environment, addr: u32) -> String {
    if addr == 0 {
        return String::new();
    }
    let ptr: ConstPtr<u8> = Ptr::from_bits(addr);
    String::from_utf8_lossy(env.mem.cstr_at(ptr)).into_owned()
}
fn node_type(env: &Environment, node: u32) -> u32 {
    rd(env, node, N_TYPE)
}
fn node_name(env: &Environment, node: u32) -> String {
    read_cstr(env, rd(env, node, N_NAME))
}
fn text_name(env: &mut Environment) -> u32 {
    if State::get_mut(env).text_name == 0 {
        let name = new_cstr(env, "text");
        State::get_mut(env).text_name = name;
    }
    State::get_mut(env).text_name
}
fn comment_name(env: &mut Environment) -> u32 {
    if State::get_mut(env).comment_name == 0 {
        let name = new_cstr(env, "comment");
        State::get_mut(env).comment_name = name;
    }
    State::get_mut(env).comment_name
}

/// Iterate a sibling list starting at `first`.
fn siblings(env: &Environment, first: u32) -> Vec<u32> {
    let mut res = Vec::new();
    let mut cur = first;
    while cur != 0 {
        res.push(cur);
        cur = rd(env, cur, N_NEXT);
    }
    res
}
fn children(env: &Environment, node: u32) -> Vec<u32> {
    match node_type(env, node) {
        XML_ELEMENT_NODE | XML_DOCUMENT_NODE | XML_ATTRIBUTE_NODE => {
            siblings(env, rd(env, node, N_CHILDREN))
        }
        _ => Vec::new(),
    }
}
fn attributes(env: &Environment, node: u32) -> Vec<u32> {
    if node_type(env, node) == XML_ELEMENT_NODE {
        siblings(env, rd(env, node, N_PROPERTIES))
    } else {
        Vec::new()
    }
}
fn descendants(env: &Environment, node: u32, out: &mut Vec<u32>) {
    if node_type(env, node) == XML_ATTRIBUTE_NODE {
        return;
    }
    for child in children(env, node) {
        out.push(child);
        descendants(env, child, out);
    }
}

/// Append `child` to the end of `parent`'s child list.
fn append_child(env: &mut Environment, parent: u32, child: u32) {
    wr(env, child, N_PARENT, parent);
    let last = rd(env, parent, N_LAST);
    if last == 0 {
        wr(env, parent, N_CHILDREN, child);
    } else {
        wr(env, last, N_NEXT, child);
        wr(env, child, N_PREV, last);
    }
    wr(env, parent, N_LAST, child);
}
/// Remove `node` from its parent's child list, if it has one.
fn unlink(env: &mut Environment, node: u32) {
    let parent = rd(env, node, N_PARENT);
    let next = rd(env, node, N_NEXT);
    let prev = rd(env, node, N_PREV);
    if prev != 0 {
        wr(env, prev, N_NEXT, next);
    } else if parent != 0 && rd(env, parent, N_CHILDREN) == node {
        wr(env, parent, N_CHILDREN, next);
    }
    if next != 0 {
        wr(env, next, N_PREV, prev);
    } else if parent != 0 && rd(env, parent, N_LAST) == node {
        wr(env, parent, N_LAST, prev);
    }
    wr(env, node, N_PARENT, 0);
    wr(env, node, N_NEXT, 0);
    wr(env, node, N_PREV, 0);
}
fn set_doc_recursive(env: &mut Environment, node: u32, doc: u32) {
    wr(env, node, N_DOC, doc);
    for attr in attributes(env, node) {
        wr(env, attr, N_DOC, doc);
        for child in children(env, attr) {
            wr(env, child, N_DOC, doc);
        }
    }
    for child in children(env, node) {
        set_doc_recursive(env, child, doc);
    }
}

fn new_node(env: &mut Environment, type_: u32, name: u32, doc: u32) -> u32 {
    let node = calloc(env, SIZEOF_NODE);
    wr(env, node, N_TYPE, type_);
    wr(env, node, N_NAME, name);
    wr(env, node, N_DOC, doc);
    node
}
fn new_text_node(env: &mut Environment, type_: u32, content: &str, doc: u32) -> u32 {
    let name = if type_ == XML_COMMENT_NODE {
        comment_name(env)
    } else {
        text_name(env)
    };
    let node = new_node(env, type_, name, doc);
    let content = new_cstr(env, content);
    wr(env, node, N_CONTENT, content);
    node
}
fn new_ns(env: &mut Environment, node: u32, href: Option<&str>, prefix: Option<&str>) -> u32 {
    let ns = calloc(env, SIZEOF_NS);
    wr(env, ns, NS_TYPE, XML_NAMESPACE_DECL);
    if let Some(href) = href {
        let href = new_cstr(env, href);
        wr(env, ns, NS_HREF, href);
    }
    if let Some(prefix) = prefix {
        let prefix = new_cstr(env, prefix);
        wr(env, ns, NS_PREFIX, prefix);
    }
    if node != 0 {
        // Append to the element's list of namespace definitions.
        let mut last = rd(env, node, N_NSDEF);
        if last == 0 {
            wr(env, node, N_NSDEF, ns);
        } else {
            while rd(env, last, NS_NEXT) != 0 {
                last = rd(env, last, NS_NEXT);
            }
            wr(env, last, NS_NEXT, ns);
        }
    }
    ns
}
/// Find the namespace bound to `prefix` (`None` is the default namespace) in
/// scope at the element `node`.
fn lookup_ns(env: &Environment, node: u32, prefix: Option<&str>) -> u32 {
    let mut cur = node;
    while cur != 0 && node_type(env, cur) == XML_ELEMENT_NODE {
        let mut ns = rd(env, cur, N_NSDEF);
        while ns != 0 {
            let ns_prefix = rd(env, ns, NS_PREFIX);
            let matches = match prefix {
                None => ns_prefix == 0,
                Some(prefix) => ns_prefix != 0 && read_cstr(env, ns_prefix) == prefix,
            };
            if matches {
                return ns;
            }
            ns = rd(env, ns, NS_NEXT);
        }
        cur = rd(env, cur, N_PARENT);
    }
    0
}

fn free_node(env: &mut Environment, node: u32) {
    let type_ = node_type(env, node);
    for child in children(env, node) {
        free_node(env, child);
    }
    if type_ == XML_ELEMENT_NODE {
        for attr in attributes(env, node) {
            free_node(env, attr);
        }
        let mut ns = rd(env, node, N_NSDEF);
        while ns != 0 {
            let next = rd(env, ns, NS_NEXT);
            let (href, prefix) = (rd(env, ns, NS_HREF), rd(env, ns, NS_PREFIX));
            free(env, href);
            free(env, prefix);
            free(env, ns);
            ns = next;
        }
    }
    match type_ {
        XML_ELEMENT_NODE | XML_ATTRIBUTE_NODE | XML_PI_NODE => {
            let name = rd(env, node, N_NAME);
            free(env, name);
        }
        _ => (), // shared static name
    }
    match type_ {
        XML_TEXT_NODE | XML_CDATA_SECTION_NODE | XML_COMMENT_NODE | XML_PI_NODE => {
            let content = rd(env, node, N_CONTENT);
            free(env, content);
        }
        _ => (),
    }
    free(env, node);
}

/// The XPath "string-value" of a node.
fn string_value(env: &Environment, node: u32) -> String {
    match node_type(env, node) {
        XML_TEXT_NODE | XML_CDATA_SECTION_NODE | XML_COMMENT_NODE | XML_PI_NODE => {
            read_cstr(env, rd(env, node, N_CONTENT))
        }
        XML_ATTRIBUTE_NODE => children(env, node)
            .into_iter()
            .map(|child| read_cstr(env, rd(env, child, N_CONTENT)))
            .collect(),
        _ => {
            let mut all = Vec::new();
            descendants(env, node, &mut all);
            all.into_iter()
                .filter(|&n| matches!(node_type(env, n), XML_TEXT_NODE | XML_CDATA_SECTION_NODE))
                .map(|n| read_cstr(env, rd(env, n, N_CONTENT)))
                .collect()
        }
    }
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

enum ParsedNode {
    Element {
        name: String,
        attrs: Vec<(String, String)>,
        children: Vec<ParsedNode>,
    },
    Text(String),
    CData(String),
    Comment(String),
}

fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp..];
        let Some(semi) = rest.find(';').filter(|&semi| semi <= 12) else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let entity = &rest[1..semi];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => {
                let code = if let Some(hex) = entity
                    .strip_prefix("#x")
                    .or_else(|| entity.strip_prefix("#X"))
                {
                    u32::from_str_radix(hex, 16).ok()
                } else if let Some(dec) = entity.strip_prefix('#') {
                    dec.parse::<u32>().ok()
                } else {
                    None
                };
                code.and_then(char::from_u32)
            }
        };
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &rest[semi + 1..];
            }
            None => {
                // Unknown entity: keep it as-is.
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

struct Parser<'a> {
    s: &'a str,
    i: usize,
}
impl Parser<'_> {
    fn rest(&self) -> &str {
        &self.s[self.i..]
    }
    fn skip_whitespace(&mut self) {
        let trimmed = self.rest().trim_start();
        self.i = self.s.len() - trimmed.len();
    }
    /// Skip past the next occurrence of `end`, returning what was before it.
    fn take_until(&mut self, end: &str) -> Result<&str, String> {
        let rest = &self.s[self.i..];
        let pos = rest
            .find(end)
            .ok_or_else(|| format!("expected {:?} after offset {}", end, self.i))?;
        self.i += pos + end.len();
        Ok(&rest[..pos])
    }

    fn parse_content(
        &mut self,
        out: &mut Vec<ParsedNode>,
        closing: Option<&str>,
    ) -> Result<(), String> {
        loop {
            let rest = self.rest();
            if rest.is_empty() {
                return match closing {
                    Some(name) => Err(format!("missing closing tag for <{}>", name)),
                    None => Ok(()),
                };
            }
            if rest.starts_with("<!--") {
                self.i += 4;
                let comment = self.take_until("-->")?.to_string();
                out.push(ParsedNode::Comment(comment));
            } else if rest.starts_with("<![CDATA[") {
                self.i += 9;
                let data = self.take_until("]]>")?.to_string();
                out.push(ParsedNode::CData(data));
            } else if rest.starts_with("<?") {
                // XML declaration or processing instruction: ignored.
                self.take_until("?>")?;
            } else if rest.starts_with("<!") {
                // DOCTYPE etc: skipped, taking care of an internal subset.
                let mut depth = 0;
                let mut end = None;
                for (pos, c) in rest.char_indices() {
                    match c {
                        '[' => depth += 1,
                        ']' => depth -= 1,
                        '>' if depth <= 0 => {
                            end = Some(pos);
                            break;
                        }
                        _ => (),
                    }
                }
                self.i += end.ok_or("unterminated <! declaration")? + 1;
            } else if rest.starts_with("</") {
                self.i += 2;
                let name = self.take_until(">")?.trim().to_string();
                return match closing {
                    Some(expected) if expected == name => Ok(()),
                    _ => Err(format!("unexpected closing tag </{}>", name)),
                };
            } else if rest.starts_with('<') {
                self.i += 1;
                out.push(self.parse_element()?);
            } else {
                let len = rest.find('<').unwrap_or(rest.len());
                let text = decode_entities(&rest[..len]);
                self.i += len;
                out.push(ParsedNode::Text(text));
            }
        }
    }

    /// Parse an element, starting just after its `<`.
    fn parse_element(&mut self) -> Result<ParsedNode, String> {
        let rest = self.rest();
        let name_len = rest
            .find(|c: char| c.is_whitespace() || c == '/' || c == '>')
            .ok_or("unterminated start tag")?;
        let name = rest[..name_len].to_string();
        if name.is_empty() {
            return Err(format!("empty element name at offset {}", self.i));
        }
        self.i += name_len;

        let mut attrs = Vec::new();
        loop {
            self.skip_whitespace();
            let rest = self.rest();
            if rest.starts_with("/>") {
                self.i += 2;
                return Ok(ParsedNode::Element {
                    name,
                    attrs,
                    children: Vec::new(),
                });
            }
            if rest.starts_with('>') {
                self.i += 1;
                let mut children = Vec::new();
                self.parse_content(&mut children, Some(&name))?;
                return Ok(ParsedNode::Element {
                    name,
                    attrs,
                    children,
                });
            }
            let key_len = rest
                .find(|c: char| c.is_whitespace() || c == '=' || c == '>' || c == '/')
                .ok_or("unterminated attribute")?;
            let key = rest[..key_len].to_string();
            if key.is_empty() {
                return Err(format!("malformed start tag <{}>", name));
            }
            self.i += key_len;
            self.skip_whitespace();
            if !self.rest().starts_with('=') {
                return Err(format!("attribute {:?} has no value", key));
            }
            self.i += 1;
            self.skip_whitespace();
            let quote = self.rest().chars().next().ok_or("unterminated attribute")?;
            if quote != '"' && quote != '\'' {
                return Err(format!("attribute {:?} value is not quoted", key));
            }
            self.i += 1;
            let value = decode_entities(self.take_until(&quote.to_string())?);
            attrs.push((key, value));
        }
    }
}

fn split_qname(qname: &str) -> (Option<&str>, &str) {
    match qname.split_once(':') {
        Some((prefix, local)) if !prefix.is_empty() && !local.is_empty() => (Some(prefix), local),
        _ => (None, qname),
    }
}

fn build_node(env: &mut Environment, doc: u32, parent: u32, parsed: &ParsedNode) {
    match parsed {
        ParsedNode::Text(text) => {
            let node = new_text_node(env, XML_TEXT_NODE, text, doc);
            append_child(env, parent, node);
        }
        ParsedNode::CData(text) => {
            let node = new_text_node(env, XML_CDATA_SECTION_NODE, text, doc);
            append_child(env, parent, node);
        }
        ParsedNode::Comment(text) => {
            let node = new_text_node(env, XML_COMMENT_NODE, text, doc);
            append_child(env, parent, node);
        }
        ParsedNode::Element {
            name,
            attrs,
            children,
        } => {
            let (prefix, local) = split_qname(name);
            let name_ptr = new_cstr(env, local);
            let node = new_node(env, XML_ELEMENT_NODE, name_ptr, doc);
            // The node has to be attached before namespaces are looked up.
            append_child(env, parent, node);

            // Namespace declarations first, they apply to the element itself.
            for (key, value) in attrs {
                if key == "xmlns" {
                    new_ns(env, node, Some(value), None);
                } else if let Some(ns_prefix) = key.strip_prefix("xmlns:") {
                    new_ns(env, node, Some(value), Some(ns_prefix));
                }
            }
            let ns = lookup_ns(env, node, prefix);
            wr(env, node, N_NS, ns);

            let mut last_attr = 0;
            for (key, value) in attrs {
                if key == "xmlns" || key.starts_with("xmlns:") {
                    continue;
                }
                let (attr_prefix, attr_local) = split_qname(key);
                let attr = calloc(env, SIZEOF_ATTR);
                let attr_name = new_cstr(env, attr_local);
                wr(env, attr, N_TYPE, XML_ATTRIBUTE_NODE);
                wr(env, attr, N_NAME, attr_name);
                wr(env, attr, N_PARENT, node);
                wr(env, attr, N_DOC, doc);
                if attr_prefix.is_some() {
                    let attr_ns = lookup_ns(env, node, attr_prefix);
                    wr(env, attr, N_NS, attr_ns);
                }
                let value_node = new_text_node(env, XML_TEXT_NODE, value, doc);
                append_child(env, attr, value_node);
                if last_attr == 0 {
                    wr(env, node, N_PROPERTIES, attr);
                } else {
                    wr(env, last_attr, N_NEXT, attr);
                    wr(env, attr, N_PREV, last_attr);
                }
                last_attr = attr;
            }

            for child in children {
                build_node(env, doc, node, child);
            }
        }
    }
}

fn new_doc(env: &mut Environment, version: &str) -> u32 {
    let doc = calloc(env, SIZEOF_DOC);
    wr(env, doc, N_TYPE, XML_DOCUMENT_NODE);
    wr(env, doc, N_DOC, doc);
    wr(env, doc, D_STANDALONE, -1i32 as u32);
    let version = new_cstr(env, version);
    wr(env, doc, D_VERSION, version);
    doc
}

fn parse_document(env: &mut Environment, bytes: Vec<u8>) -> u32 {
    let text = String::from_utf8_lossy(&bytes);
    let text = text.trim_start_matches('\u{feff}');
    log_dbg!("libxml2: parsing document: {:?}", text);

    let mut parser = Parser { s: text, i: 0 };
    let mut top_level = Vec::new();
    if let Err(e) = parser.parse_content(&mut top_level, None) {
        log!("Warning: libxml2: could not parse document: {}", e);
        return 0;
    }
    if !top_level
        .iter()
        .any(|n| matches!(n, ParsedNode::Element { .. }))
    {
        log!("Warning: libxml2: document has no root element");
        return 0;
    }

    let doc = new_doc(env, "1.0");
    let encoding = new_cstr(env, "UTF-8");
    wr(env, doc, D_ENCODING, encoding);
    for node in &top_level {
        // Text is not allowed outside the root element.
        if !matches!(node, ParsedNode::Text(_) | ParsedNode::CData(_)) {
            build_node(env, doc, doc, node);
        }
    }
    doc
}

fn xmlReadMemory(
    env: &mut Environment,
    buffer: ConstPtr<u8>,
    size: i32,
    _url: ConstPtr<u8>,
    _encoding: ConstPtr<u8>,
    _options: i32,
) -> u32 {
    if buffer.is_null() || size <= 0 {
        return 0;
    }
    let bytes = env.mem.bytes_at(buffer, size as GuestUSize).to_vec();
    parse_document(env, bytes)
}

fn xmlParseDoc(env: &mut Environment, cur: ConstPtr<u8>) -> u32 {
    if cur.is_null() {
        return 0;
    }
    let bytes = env.mem.cstr_at(cur).to_vec();
    parse_document(env, bytes)
}

fn xmlFreeDoc(env: &mut Environment, doc: u32) {
    log_dbg!("xmlFreeDoc({:#x})", doc);
    if doc == 0 {
        return;
    }
    for child in children(env, doc) {
        free_node(env, child);
    }
    let (version, encoding) = (rd(env, doc, D_VERSION), rd(env, doc, D_ENCODING));
    free(env, version);
    free(env, encoding);
    free(env, doc);
}

// ---------------------------------------------------------------------------
// Tree access and construction
// ---------------------------------------------------------------------------

fn xmlDocGetRootElement(env: &mut Environment, doc: u32) -> u32 {
    log_dbg!("xmlDocGetRootElement({:#x})", doc);
    if doc == 0 {
        return 0;
    }
    children(env, doc)
        .into_iter()
        .find(|&n| node_type(env, n) == XML_ELEMENT_NODE)
        .unwrap_or(0)
}

fn xmlDocSetRootElement(env: &mut Environment, doc: u32, root: u32) -> u32 {
    if doc == 0 {
        return 0;
    }
    let old = xmlDocGetRootElement(env, doc);
    if old != 0 {
        unlink(env, old);
    }
    if root != 0 {
        unlink(env, root);
        set_doc_recursive(env, root, doc);
        append_child(env, doc, root);
    }
    old
}

fn xmlNewDoc(env: &mut Environment, version: ConstPtr<u8>) -> u32 {
    let version = if version.is_null() {
        "1.0".to_string()
    } else {
        read_cstr(env, version.to_bits())
    };
    new_doc(env, &version)
}

fn xmlNewNode(env: &mut Environment, ns: u32, name: ConstPtr<u8>) -> u32 {
    if name.is_null() {
        return 0;
    }
    let name = read_cstr(env, name.to_bits());
    let name = new_cstr(env, &name);
    let node = new_node(env, XML_ELEMENT_NODE, name, 0);
    wr(env, node, N_NS, ns);
    node
}

fn xmlNewText(env: &mut Environment, content: ConstPtr<u8>) -> u32 {
    let content = read_cstr(env, content.to_bits());
    new_text_node(env, XML_TEXT_NODE, &content, 0)
}

fn xmlNewPI(env: &mut Environment, name: ConstPtr<u8>, content: ConstPtr<u8>) -> u32 {
    if name.is_null() {
        return 0;
    }
    let name = read_cstr(env, name.to_bits());
    let name = new_cstr(env, &name);
    let node = new_node(env, XML_PI_NODE, name, 0);
    if !content.is_null() {
        let content = read_cstr(env, content.to_bits());
        let content = new_cstr(env, &content);
        wr(env, node, N_CONTENT, content);
    }
    node
}

fn xmlAddChild(env: &mut Environment, parent: u32, cur: u32) -> u32 {
    if parent == 0 || cur == 0 {
        return 0;
    }
    unlink(env, cur);
    let doc = rd(env, parent, N_DOC);
    set_doc_recursive(env, cur, doc);
    append_child(env, parent, cur);
    cur
}

fn xmlAddPrevSibling(env: &mut Environment, cur: u32, elem: u32) -> u32 {
    if cur == 0 || elem == 0 {
        return 0;
    }
    unlink(env, elem);
    let parent = rd(env, cur, N_PARENT);
    let prev = rd(env, cur, N_PREV);
    let doc = rd(env, cur, N_DOC);
    set_doc_recursive(env, elem, doc);
    wr(env, elem, N_PARENT, parent);
    wr(env, elem, N_NEXT, cur);
    wr(env, elem, N_PREV, prev);
    wr(env, cur, N_PREV, elem);
    if prev != 0 {
        wr(env, prev, N_NEXT, elem);
    } else if parent != 0 {
        wr(env, parent, N_CHILDREN, elem);
    }
    elem
}

fn xmlNewNs(env: &mut Environment, node: u32, href: ConstPtr<u8>, prefix: ConstPtr<u8>) -> u32 {
    let href = (!href.is_null()).then(|| read_cstr(env, href.to_bits()));
    let prefix = (!prefix.is_null()).then(|| read_cstr(env, prefix.to_bits()));
    new_ns(env, node, href.as_deref(), prefix.as_deref())
}

fn xmlSetNs(env: &mut Environment, node: u32, ns: u32) {
    if node != 0 {
        wr(env, node, N_NS, ns);
    }
}

fn xmlNodeListGetString(env: &mut Environment, _doc: u32, list: u32, _in_line: i32) -> u32 {
    log_dbg!("xmlNodeListGetString(list {:#x})", list);
    if list == 0 {
        return 0;
    }
    let mut res = String::new();
    let mut found = false;
    for node in siblings(env, list) {
        if matches!(
            node_type(env, node),
            XML_TEXT_NODE | XML_CDATA_SECTION_NODE
        ) {
            found = true;
            res.push_str(&read_cstr(env, rd(env, node, N_CONTENT)));
        }
    }
    if found {
        new_cstr(env, &res)
    } else {
        0
    }
}

fn xmlStrcmp(env: &mut Environment, a: ConstPtr<u8>, b: ConstPtr<u8>) -> i32 {
    log_dbg!("xmlStrcmp({:?}, {:?})", a, b);
    if a == b {
        return 0;
    }
    if a.is_null() {
        return -1;
    }
    if b.is_null() {
        return 1;
    }
    match env.mem.cstr_at(a).cmp(env.mem.cstr_at(b)) {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    }
}

/// `xmlFree` is a function pointer variable in libxml2, not a function, so
/// this is what we make that variable point to.
fn touchHLE_xmlFree(env: &mut Environment, ptr: MutVoidPtr) {
    log_dbg!("xmlFree({:?})", ptr);
    if !ptr.is_null() {
        env.mem.free(ptr);
    }
}

// ---------------------------------------------------------------------------
// XPath (subset)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Debug)]
enum Axis {
    Child,
    Descendant,
    DescendantOrSelf,
    SelfAxis,
    Parent,
    Attribute,
}
#[derive(Debug)]
enum NodeTest {
    Name(String),
    Any,
    Text,
    Node,
}
#[derive(Debug)]
struct Step {
    axis: Axis,
    test: NodeTest,
    predicates: Vec<String>,
}

/// Split `s` on `sep`, ignoring separators inside quotes, brackets or parens.
fn split_top_level<'a>(s: &'a str, sep: &str) -> Vec<&'a str> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut start = 0;
    let mut i = 0;
    while i < s.len() {
        let c = s[i..].chars().next().unwrap();
        if let Some(q) = quote {
            if c == q {
                quote = None;
            }
        } else {
            match c {
                '\'' | '"' => quote = Some(c),
                '[' | '(' => depth += 1,
                ']' | ')' => depth -= 1,
                _ => {
                    if depth == 0 && s[i..].starts_with(sep) {
                        parts.push(&s[start..i]);
                        i += sep.len();
                        start = i;
                        continue;
                    }
                }
            }
        }
        i += c.len_utf8();
    }
    parts.push(&s[start..]);
    parts
}

fn parse_step(segment: &str) -> Result<Step, String> {
    let segment = segment.trim();
    let (base, predicates) = match segment.find('[') {
        Some(pos) => {
            let mut predicates = Vec::new();
            let mut depth = 0;
            let mut start = 0;
            let mut quote: Option<char> = None;
            for (i, c) in segment[pos..].char_indices() {
                let i = pos + i;
                if let Some(q) = quote {
                    if c == q {
                        quote = None;
                    }
                    continue;
                }
                match c {
                    '\'' | '"' => quote = Some(c),
                    '[' => {
                        if depth == 0 {
                            start = i + 1;
                        }
                        depth += 1;
                    }
                    ']' => {
                        depth -= 1;
                        if depth == 0 {
                            predicates.push(segment[start..i].trim().to_string());
                        }
                    }
                    _ => (),
                }
            }
            if depth != 0 {
                return Err(format!("unbalanced brackets in {:?}", segment));
            }
            (segment[..pos].trim(), predicates)
        }
        None => (segment, Vec::new()),
    };

    let (axis, test) = if base == "." {
        (Axis::SelfAxis, "node()")
    } else if base == ".." {
        (Axis::Parent, "node()")
    } else if let Some(name) = base.strip_prefix('@') {
        (Axis::Attribute, name)
    } else if let Some((axis, test)) = base.split_once("::") {
        let axis = match axis.trim() {
            "child" => Axis::Child,
            "descendant" => Axis::Descendant,
            "descendant-or-self" => Axis::DescendantOrSelf,
            "self" => Axis::SelfAxis,
            "parent" => Axis::Parent,
            "attribute" => Axis::Attribute,
            other => return Err(format!("unsupported axis {:?}", other)),
        };
        (axis, test)
    } else {
        (Axis::Child, base)
    };
    let test = match test.trim() {
        "*" => NodeTest::Any,
        "text()" => NodeTest::Text,
        "node()" => NodeTest::Node,
        name => {
            let (_prefix, local) = split_qname(name);
            let valid = !local.is_empty()
                && local
                    .chars()
                    .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.'));
            if !valid {
                return Err(format!("unsupported node test {:?}", name));
            }
            if local == "*" {
                NodeTest::Any
            } else {
                NodeTest::Name(local.to_string())
            }
        }
    };
    Ok(Step {
        axis,
        test,
        predicates,
    })
}

/// Evaluate a location path (possibly a union of them) relative to `context`.
fn eval_path(env: &Environment, context: u32, expr: &str) -> Result<Vec<u32>, String> {
    let mut seen = HashSet::new();
    let mut result = Vec::new();
    for path in split_top_level(expr, "|") {
        for node in eval_single_path(env, context, path.trim())? {
            if seen.insert(node) {
                result.push(node);
            }
        }
    }
    Ok(result)
}

fn eval_single_path(env: &Environment, context: u32, path: &str) -> Result<Vec<u32>, String> {
    if path.is_empty() {
        return Err("empty path".to_string());
    }
    let segments = split_top_level(path, "/");
    let mut steps = Vec::new();
    let mut absolute = false;
    let mut pending_descendant = false;
    for (i, segment) in segments.iter().enumerate() {
        if segment.trim().is_empty() {
            if i == 0 {
                absolute = true;
            } else if i != segments.len() - 1 || segments.len() == 2 && !absolute {
                pending_descendant = true;
            } else if i == segments.len() - 1 && i >= 2 && segments[i - 1].trim().is_empty() {
                return Err(format!("path ends with //: {:?}", path));
            }
            continue;
        }
        if pending_descendant {
            steps.push(Step {
                axis: Axis::DescendantOrSelf,
                test: NodeTest::Node,
                predicates: Vec::new(),
            });
            pending_descendant = false;
        }
        steps.push(parse_step(segment)?);
    }

    let start = if absolute {
        // The document node is the root of the tree.
        let doc = rd(env, context, N_DOC);
        if doc != 0 {
            doc
        } else {
            let mut root = context;
            while rd(env, root, N_PARENT) != 0 {
                root = rd(env, root, N_PARENT);
            }
            root
        }
    } else {
        context
    };

    let mut current = vec![start];
    for step in &steps {
        let mut seen = HashSet::new();
        let mut next = Vec::new();
        for &ctx in &current {
            let mut candidates: Vec<u32> = match step.axis {
                Axis::Child => children(env, ctx),
                Axis::Descendant => {
                    let mut all = Vec::new();
                    descendants(env, ctx, &mut all);
                    all
                }
                Axis::DescendantOrSelf => {
                    let mut all = vec![ctx];
                    descendants(env, ctx, &mut all);
                    all
                }
                Axis::SelfAxis => vec![ctx],
                Axis::Parent => {
                    let parent = rd(env, ctx, N_PARENT);
                    if parent != 0 {
                        vec![parent]
                    } else {
                        Vec::new()
                    }
                }
                Axis::Attribute => attributes(env, ctx),
            };
            let principal = if step.axis == Axis::Attribute {
                XML_ATTRIBUTE_NODE
            } else {
                XML_ELEMENT_NODE
            };
            candidates.retain(|&n| {
                let type_ = node_type(env, n);
                match &step.test {
                    NodeTest::Node => true,
                    NodeTest::Text => matches!(type_, XML_TEXT_NODE | XML_CDATA_SECTION_NODE),
                    NodeTest::Any => type_ == principal,
                    NodeTest::Name(name) => type_ == principal && &node_name(env, n) == name,
                }
            });
            for predicate in &step.predicates {
                candidates = filter_predicate(env, candidates, predicate)?;
            }
            for node in candidates {
                if seen.insert(node) {
                    next.push(node);
                }
            }
        }
        current = next;
    }
    Ok(current)
}

fn filter_predicate(
    env: &Environment,
    candidates: Vec<u32>,
    predicate: &str,
) -> Result<Vec<u32>, String> {
    let len = candidates.len();
    let mut result = Vec::new();
    for (i, &node) in candidates.iter().enumerate() {
        if eval_bool(env, node, i + 1, len, predicate)? {
            result.push(node);
        }
    }
    Ok(result)
}

fn strip_call<'a>(expr: &'a str, function: &str) -> Option<&'a str> {
    expr.strip_prefix(function)
        .map(str::trim_start)
        .and_then(|rest| rest.strip_prefix('('))
        .and_then(|rest| rest.strip_suffix(')'))
}

fn eval_bool(
    env: &Environment,
    node: u32,
    position: usize,
    last: usize,
    expr: &str,
) -> Result<bool, String> {
    let expr = expr.trim();
    let or_parts = split_top_level(expr, " or ");
    if or_parts.len() > 1 {
        for part in or_parts {
            if eval_bool(env, node, position, last, part)? {
                return Ok(true);
            }
        }
        return Ok(false);
    }
    let and_parts = split_top_level(expr, " and ");
    if and_parts.len() > 1 {
        for part in and_parts {
            if !eval_bool(env, node, position, last, part)? {
                return Ok(false);
            }
        }
        return Ok(true);
    }

    if let Some(inner) = strip_call(expr, "not") {
        return Ok(!eval_bool(env, node, position, last, inner)?);
    }
    for (function, test) in [
        (
            "contains",
            (|a: &str, b: &str| a.contains(b)) as fn(&str, &str) -> bool,
        ),
        ("starts-with", |a: &str, b: &str| a.starts_with(b)),
    ] {
        if let Some(args) = strip_call(expr, function) {
            let args = split_top_level(args, ",");
            if args.len() != 2 {
                return Err(format!("{}() needs two arguments", function));
            }
            let a = eval_values(env, node, position, last, args[0])?;
            let b = eval_values(env, node, position, last, args[1])?;
            let a = a.first().map(String::as_str).unwrap_or("");
            let b = b.first().map(String::as_str).unwrap_or("");
            return Ok(test(a, b));
        }
    }

    for (op, negate) in [("!=", true), ("=", false)] {
        let parts = split_top_level(expr, op);
        if parts.len() == 2 {
            let lhs = eval_values(env, node, position, last, parts[0])?;
            let rhs = eval_values(env, node, position, last, parts[1])?;
            let equal = |a: &String, b: &String| match (a.trim().parse::<f64>(), b.trim().parse::<f64>())
            {
                (Ok(a), Ok(b)) => a == b,
                _ => a == b,
            };
            let any = lhs
                .iter()
                .any(|a| rhs.iter().any(|b| equal(a, b) != negate));
            return Ok(any);
        }
    }

    // A bare number is a position test, anything else an existence test.
    if let Ok(n) = expr.parse::<f64>() {
        return Ok(n == position as f64);
    }
    if expr == "last()" {
        return Ok(position == last);
    }
    Ok(!eval_path(env, node, expr)?.is_empty())
}

/// Evaluate an operand of a comparison to the strings it could stand for.
fn eval_values(
    env: &Environment,
    node: u32,
    position: usize,
    last: usize,
    expr: &str,
) -> Result<Vec<String>, String> {
    let expr = expr.trim();
    for quote in ['\'', '"'] {
        if expr.len() >= 2 && expr.starts_with(quote) && expr.ends_with(quote) {
            return Ok(vec![expr[1..expr.len() - 1].to_string()]);
        }
    }
    if expr.parse::<f64>().is_ok() {
        return Ok(vec![expr.to_string()]);
    }
    match expr {
        "position()" => return Ok(vec![position.to_string()]),
        "last()" => return Ok(vec![last.to_string()]),
        "name()" | "local-name()" => return Ok(vec![node_name(env, node)]),
        _ => (),
    }
    if let Some(inner) = strip_call(expr, "string") {
        return eval_values(env, node, position, last, inner);
    }
    Ok(eval_path(env, node, expr)?
        .into_iter()
        .map(|n| string_value(env, n))
        .collect())
}

fn xmlXPathNewContext(env: &mut Environment, doc: u32) -> u32 {
    log_dbg!("xmlXPathNewContext({:#x})", doc);
    let ctx = calloc(env, SIZEOF_XPATH_CONTEXT);
    wr(env, ctx, XC_DOC, doc);
    ctx
}

fn xmlXPathFreeContext(env: &mut Environment, ctx: u32) {
    free(env, ctx);
}

fn xmlXPathRegisterNs(
    _env: &mut Environment,
    _ctx: u32,
    _prefix: ConstPtr<u8>,
    _ns_uri: ConstPtr<u8>,
) -> i32 {
    // Namespace prefixes in expressions are ignored: names are compared by
    // their local part only.
    0
}

fn xmlXPathEvalExpression(env: &mut Environment, expr: ConstPtr<u8>, ctx: u32) -> u32 {
    if expr.is_null() || ctx == 0 {
        return 0;
    }
    let expr = read_cstr(env, expr.to_bits());
    let context_node = match rd(env, ctx, XC_NODE) {
        0 => rd(env, ctx, XC_DOC),
        node => node,
    };
    if context_node == 0 {
        return 0;
    }
    let nodes = match eval_path(env, context_node, &expr) {
        Ok(nodes) => nodes,
        Err(e) => {
            log!(
                "Warning: xmlXPathEvalExpression({:?}): unsupported expression: {}",
                expr,
                e
            );
            return 0;
        }
    };
    log_dbg!("xmlXPathEvalExpression({:?}) => {} nodes", expr, nodes.len());

    let set = calloc(env, SIZEOF_NODE_SET);
    if !nodes.is_empty() {
        let tab = calloc(env, 4 * nodes.len() as u32);
        for (i, &node) in nodes.iter().enumerate() {
            wr(env, tab, 4 * i as u32, node);
        }
        wr(env, set, NSET_NODE_NR, nodes.len() as u32);
        wr(env, set, NSET_NODE_MAX, nodes.len() as u32);
        wr(env, set, NSET_NODE_TAB, tab);
    }
    let obj = calloc(env, SIZEOF_XPATH_OBJECT);
    wr(env, obj, XO_TYPE, XPATH_NODESET);
    wr(env, obj, XO_NODESETVAL, set);
    obj
}

fn xmlXPathFreeObject(env: &mut Environment, obj: u32) {
    if obj == 0 {
        return;
    }
    if rd(env, obj, XO_TYPE) == XPATH_NODESET {
        let set = rd(env, obj, XO_NODESETVAL);
        if set != 0 {
            let tab = rd(env, set, NSET_NODE_TAB);
            free(env, tab);
            free(env, set);
        }
    }
    free(env, obj);
}

pub const CONSTANTS: ConstantExports = &[(
    "_xmlFree",
    HostConstant::Custom(|env| -> ConstVoidPtr {
        let function = env
            .dyld
            .create_proc_address(&mut env.mem, &mut env.cpu, "_touchHLE_xmlFree")
            .unwrap();
        env.mem
            .alloc_and_write(function.addr_with_thumb_bit())
            .cast()
            .cast_const()
    }),
)];

pub const FUNCTIONS: FunctionExports = &[
    export_c_func!(xmlReadMemory(_, _, _, _, _)),
    export_c_func!(xmlParseDoc(_)),
    export_c_func!(xmlFreeDoc(_)),
    export_c_func!(xmlDocGetRootElement(_)),
    export_c_func!(xmlDocSetRootElement(_, _)),
    export_c_func!(xmlNewDoc(_)),
    export_c_func!(xmlNewNode(_, _)),
    export_c_func!(xmlNewText(_)),
    export_c_func!(xmlNewPI(_, _)),
    export_c_func!(xmlAddChild(_, _)),
    export_c_func!(xmlAddPrevSibling(_, _)),
    export_c_func!(xmlNewNs(_, _, _)),
    export_c_func!(xmlSetNs(_, _)),
    export_c_func!(xmlNodeListGetString(_, _, _)),
    export_c_func!(xmlStrcmp(_, _)),
    export_c_func!(touchHLE_xmlFree(_)),
    export_c_func!(xmlXPathNewContext(_)),
    export_c_func!(xmlXPathFreeContext(_)),
    export_c_func!(xmlXPathRegisterNs(_, _, _)),
    export_c_func!(xmlXPathEvalExpression(_, _)),
    export_c_func!(xmlXPathFreeObject(_)),
];

pub const DYLIB: HostDylib = HostDylib {
    path: "/usr/lib/libxml2.2.dylib",
    aliases: &["/usr/lib/libxml2.dylib"],
    class_exports: &[],
    constant_exports: &[CONSTANTS],
    function_exports: &[FUNCTIONS],
};
