#[macro_use]
extern crate error_chain;
extern crate fnv;
extern crate stylish_syntax as syntax;

pub mod query;
pub mod error;
mod rule;
use rule::*;
#[macro_use]
mod macros;

use fnv::FnvHashMap;

/// The error type used by stylish
pub type SResult<T> = error::Result<T>;
use error::ErrorKind;

use std::rc::{Rc, Weak};
use std::cell::{Ref, RefCell};
use std::any::Any;

pub use syntax::{format_error, format_parse_error};

/// Stores loaded nodes and manages the layout.
pub struct Manager<RInfo> {
    // Has no parent, is the parent for all base nodes
    // in the system
    root: Node<RInfo>,
    styles: Styles<RInfo>,
    last_size: (i32, i32),
    dirty: bool,
}

impl<RInfo> Manager<RInfo> {
    /// Creates a new manager with an empty root node.
    pub fn new() -> Manager<RInfo> {
        Manager {
            root: Node::root(),
            styles: Styles {
                styles: Vec::new(),
                layouts: {
                    let mut layouts: FnvHashMap<
                        String,
                        Box<
                            Fn(&RenderObject<RInfo>)
                                -> Box<LayoutEngine<RInfo>>,
                        >,
                    > = FnvHashMap::default();
                    layouts.insert(
                        "absolute".to_owned(),
                        Box::new(|_| Box::new(AbsoluteLayout)),
                    );
                    layouts
                },
                funcs: FnvHashMap::default(),
                rules_by_base: FnvHashMap::default(),
            },
            last_size: (0, 0),
            dirty: true,
        }
    }

    /// Adds a new function that can be used to create a layout engine.
    ///
    /// A layout engine is used to position elements within an element.
    ///
    /// The layout engine can be selected by using the `layout` attribute.
    pub fn add_layout_engine<F>(&mut self, name: &str, creator: F)
    where
        F: Fn(&RenderObject<RInfo>) -> Box<LayoutEngine<RInfo>> + 'static,
    {
        self.styles.layouts.insert(name.into(), Box::new(creator));
    }

    /// Add a function that can be called by styles
    pub fn add_func_raw<F>(&mut self, name: &str, func: F)
    where
        F: Fn(Vec<Value>) -> SResult<Value> + 'static,
    {
        self.styles.funcs.insert(name.into(), Box::new(func));
    }

    /// Adds the node to the root node of this manager.
    ///
    /// The node is created from the passed string.
    /// See [`add_node_str`](struct.Node.html#from_str)
    pub fn add_node_str<'a>(&mut self, node: &'a str) -> Result<(), syntax::PError<'a>> {
        self.add_node(Node::from_str(node)?);
        Ok(())
    }

    /// Adds the node to the root node of this manager
    pub fn add_node(&mut self, node: Node<RInfo>) {
        self.root.add_child(node);
    }

    /// Removes the node from the root node of this manager
    pub fn remove_node(&mut self, node: Node<RInfo>) {
        self.root.remove_child(node);
        self.dirty = true;
    }

    /// Starts a query from the root of this manager
    pub fn query(&self) -> query::Query<RInfo> {
        query::Query::new(self.root.clone())
    }

    /// Starts a query looking for elements at the target
    /// location.
    pub fn query_at(&self, x: i32, y: i32) -> query::Query<RInfo> {
        query::Query {
            root: self.root.clone(),
            rules: Vec::new(),
            location: Some(query::AtLocation { x: x, y: y }),
        }
    }

    /// Loads a set of styles from the given string.
    ///
    /// If a set of styles with the same name is already loaded
    /// then this will replace them.
    pub fn load_styles<'a>(
        &mut self,
        name: &str,
        style_rules: &'a str,
    ) -> Result<(), syntax::PError<'a>> {
        let styles = syntax::style::Document::parse(style_rules)?;
        self.styles.styles.retain(|v| v.0 != name);
        self.styles.styles.push((name.into(), styles));
        self.dirty = true;
        self.rebuild_styles();
        Ok(())
    }

    /// Removes the set of styles with the given name
    pub fn remove_styles(&mut self, name: &str) {
        self.styles.styles.retain(|v| v.0 != name);
        self.dirty = true;
        self.rebuild_styles();
    }

    fn rebuild_styles(&mut self) {
        self.styles.rules_by_base.clear();
        for doc in &self.styles.styles {
            for rule in &doc.1.rules {
                let m = if let Some(m) = rule.matchers.last() {
                    match m.0 {
                        syntax::style::Matcher::Element(ref e) => {
                            Matcher::Element(e.name.name.clone())
                        }
                        syntax::style::Matcher::Text => Matcher::Text,
                    }
                } else {
                    continue;
                };
                self.styles
                    .rules_by_base
                    .entry(m)
                    .or_insert_with(Vec::new)
                    .push(rule.clone());
            }
        }
    }

    /// Positions the nodes in this manager.
    pub fn layout(&mut self, width: i32, height: i32) -> bool {
        let force_dirty = self.last_size != (width, height) || self.dirty;
        self.dirty = false;
        self.last_size = (width, height);
        self.root.set_property("width", width);
        self.root.set_property("height", height);
        {
            let mut inner = self.root.inner.borrow_mut();
            inner.render_object = Some(RenderObject {
                draw_rect: Rect {
                    x: 0,
                    y: 0,
                    width: width,
                    height: height,
                },
                ..RenderObject::default()
            });
        }
        let inner = self.root.inner.borrow();
        if let NodeValue::Element(ref e) = inner.value {
            let mut dirty = force_dirty;
            for c in &e.children {
                if c.check_dirty() {
                    dirty = true;
                    c.inner.borrow_mut().render_object = None;
                }
            }
            if dirty {
                for c in &e.children {
                    c.layout(&self.styles, &mut AbsoluteLayout, force_dirty);
                }
                true
            } else {
                false
            }
        } else {
            false
        }
    }

    /// Renders the nodes in this manager by passing the
    /// layout and styles to the passed visitor.
    pub fn render<V>(&mut self, visitor: &mut V)
    where
        V: RenderVisitor<RInfo>,
    {
        let inner = self.root.inner.borrow();
        if let NodeValue::Element(ref e) = inner.value {
            for c in &e.children {
                c.render(&self.styles, visitor);
            }
        }
    }
}

/// Used to position an element within another element.
pub trait LayoutEngine<RInfo> {
    fn pre_position_child(&mut self, obj: &mut RenderObject<RInfo>, parent: &RenderObject<RInfo>);
    fn post_position_child(&mut self, obj: &mut RenderObject<RInfo>, parent: &RenderObject<RInfo>);

    /// Runs on the element not the children
    fn finalize_layout(
        &mut self,
        obj: &mut RenderObject<RInfo>,
        children: Vec<&mut RenderObject<RInfo>>,
    );
}

impl<RInfo> LayoutEngine<RInfo> for Box<LayoutEngine<RInfo>> {
    fn pre_position_child(&mut self, obj: &mut RenderObject<RInfo>, parent: &RenderObject<RInfo>) {
        (**self).pre_position_child(obj, parent)
    }
    fn post_position_child(&mut self, obj: &mut RenderObject<RInfo>, parent: &RenderObject<RInfo>) {
        (**self).post_position_child(obj, parent)
    }
    fn finalize_layout(
        &mut self,
        obj: &mut RenderObject<RInfo>,
        children: Vec<&mut RenderObject<RInfo>>,
    ) {
        (**self).finalize_layout(obj, children)
    }
}

/// The default layout.
///
/// Copies the values of `x`, `y`, `width` and `height` directly
/// to the element's layout.
struct AbsoluteLayout;

impl<RInfo> LayoutEngine<RInfo> for AbsoluteLayout {
    fn pre_position_child(&mut self, obj: &mut RenderObject<RInfo>, _parent: &RenderObject<RInfo>) {
        let width = obj.get_value::<i32>("width");
        let height = obj.get_value::<i32>("height");
        obj.draw_rect = Rect {
            x: obj.get_value::<i32>("x").unwrap_or(0),
            y: obj.get_value::<i32>("y").unwrap_or(0),
            width: width
                .or_else(|| obj.get_value::<i32>("min_width"))
                .unwrap_or(0),
            height: height
                .or_else(|| obj.get_value::<i32>("min_height"))
                .unwrap_or(0),
        };
        obj.min_size = (obj.draw_rect.width, obj.draw_rect.height);
        obj.max_size = (
            width.or_else(|| obj.get_value::<i32>("max_width")),
            height.or_else(|| obj.get_value::<i32>("max_height")),
        );
    }

    fn post_position_child(
        &mut self,
        _obj: &mut RenderObject<RInfo>,
        _parent: &RenderObject<RInfo>,
    ) {
    }

    fn finalize_layout(
        &mut self,
        obj: &mut RenderObject<RInfo>,
        children: Vec<&mut RenderObject<RInfo>>,
    ) {
        use std::cmp;
        if !obj.get_value::<bool>("auto_size").unwrap_or(false) {
            return;
        }
        let mut max = obj.min_size;
        for c in children {
            max.0 = cmp::max(max.0, c.draw_rect.x + c.draw_rect.width);
            max.1 = cmp::max(max.1, c.draw_rect.y + c.draw_rect.height);
        }
        if let Some(v) = obj.max_size.0 {
            max.0 = cmp::min(v, max.0);
        }
        if let Some(v) = obj.max_size.1 {
            max.1 = cmp::min(v, max.1);
        }
        obj.draw_rect.width = max.0;
        obj.draw_rect.height = max.1;
    }
}

/// The position and size of an element
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// Called for every element in a manager to allow them to
/// be rendered.
pub trait RenderVisitor<RInfo> {
    /// Called with an element to be rendered.
    fn visit(&mut self, obj: &mut RenderObject<RInfo>);
    /// Called after all of the passed element's children
    /// have been visited.
    fn visit_end(&mut self, _obj: &mut RenderObject<RInfo>) {}
}

#[derive(Debug, PartialEq, Eq, Clone, Hash)]
enum Matcher {
    Element(String),
    Text,
}

struct Styles<RInfo> {
    styles: Vec<(String, syntax::style::Document)>,
    layouts: FnvHashMap<String, Box<Fn(&RenderObject<RInfo>) -> Box<LayoutEngine<RInfo>>>>,
    funcs: FnvHashMap<String, Box<Fn(Vec<Value>) -> SResult<Value>>>,

    rules_by_base: FnvHashMap<Matcher, Vec<syntax::style::Rule>>,
}

impl<RInfo> Styles<RInfo> {
    // TODO: Remove boxing
    fn find_matching_rules<'a, 'b>(
        &'a self,
        node: &'b Node<RInfo>,
    ) -> RuleIter<'b, Box<Iterator<Item = &'a syntax::style::Rule> + 'a>, RInfo> {
        use std::iter;
        let iter = self.rules_by_base
            .get(&node.name().map(Matcher::Element).unwrap_or(Matcher::Text))
            .map(|v| v.iter().rev())
            .map(|v| Box::new(v) as Box<_>)
            .unwrap_or_else(|| Box::new(iter::empty()) as Box<_>);
        RuleIter {
            node: node,
            rules: iter,
        }
    }
}

/// A node representing an element.
///
/// Can be cloned to duplicate the reference to the node.
pub struct Node<RInfo> {
    inner: Rc<RefCell<NodeInner<RInfo>>>,
}

impl<RInfo> Clone for Node<RInfo> {
    fn clone(&self) -> Self {
        Node {
            inner: self.inner.clone(),
        }
    }
}

impl<RInfo> Node<RInfo> {
    fn check_dirty(&self) -> bool {
        {
            let inner = self.inner.borrow();
            if inner.dirty {
                return true;
            }
            if let NodeValue::Element(ref e) = inner.value {
                for c in &e.children {
                    if c.check_dirty() {
                        return true;
                    }
                }
            }
        }
        false
    }

    fn layout<L>(&self, styles: &Styles<RInfo>, layout: &mut L, force_dirty: bool)
    where
        L: LayoutEngine<RInfo>,
    {
        use std::collections::hash_map::Entry;
        use std::mem;
        let mut dirty = force_dirty;
        {
            let missing_obj = {
                let inner = self.inner.borrow();
                inner.render_object.is_none()
            };
            if missing_obj || force_dirty {
                dirty = true;
                let mut obj = RenderObject::default();
                let parent_rect = if let Some(parent) = self.inner
                    .borrow()
                    .parent
                    .as_ref()
                    .and_then(|v| v.upgrade())
                {
                    let parent = parent.borrow();
                    parent.render_object.as_ref().unwrap().draw_rect
                } else {
                    Rect {
                        x: 0,
                        y: 0,
                        width: 0,
                        height: 0,
                    }
                };
                let mut scroll_x_set = false;
                let mut scroll_y_set = false;
                let mut clip_overflow_set = false;
                for rule in styles.find_matching_rules(self) {
                    for key in rule.syn.styles.keys() {
                        let key = key.name.as_str();
                        match key {
                            "scroll_x" => if !scroll_x_set {
                                if let Some(v) = rule.get_value(styles, parent_rect, key) {
                                    scroll_x_set = true;
                                    obj.scroll_position.0 = v;
                                }
                            },
                            "scroll_y" => if !scroll_y_set {
                                if let Some(v) = rule.get_value(styles, parent_rect, key) {
                                    scroll_y_set = true;
                                    obj.scroll_position.1 = v;
                                }
                            },
                            "clip_overflow" => if !clip_overflow_set {
                                if let Some(v) = rule.get_value(styles, parent_rect, key) {
                                    clip_overflow_set = true;
                                    obj.clip_overflow = v;
                                }
                            },
                            _ => if let Entry::Vacant(e) = obj.vars.entry(key.to_owned()) {
                                if let Some(v) = rule.get_value(styles, parent_rect, key) {
                                    e.insert(v);
                                }
                            },
                        }
                    }
                }
                let mut inner = self.inner.borrow_mut();
                if let Some(parent) = inner.parent.as_ref().and_then(|v| v.upgrade()) {
                    let parent = parent.borrow();
                    layout.pre_position_child(&mut obj, parent.render_object.as_ref().unwrap());
                }
                if let Some(layout) = obj.get_value::<String>("layout") {
                    if let Some(engine) = styles.layouts.get(&layout) {
                        obj.layout_engine = RefCell::new(engine(&obj));
                    }
                }
                if let NodeValue::Text(ref txt) = inner.value {
                    obj.text = Some(txt.clone());
                }
                inner.dirty = false;
                inner.render_object = Some(obj);
            }
        }
        {
            let inner = self.inner.borrow();
            if let Some(render) = inner.render_object.as_ref() {
                let mut layout_engine = render.layout_engine.borrow_mut();
                if let NodeValue::Element(ref e) = inner.value {
                    for c in &e.children {
                        c.layout(styles, &mut *layout_engine, dirty);
                    }
                }
            }
        }
        if dirty {
            let inner: &mut NodeInner<RInfo> = &mut *self.inner.borrow_mut();
            if let Some(render) = inner.render_object.as_mut() {
                let layout_engine = mem::replace(
                    &mut render.layout_engine,
                    RefCell::new(Box::new(AbsoluteLayout)),
                );
                if let NodeValue::Element(ref e) = inner.value {
                    let mut children_ref = e.children
                        .iter()
                        .map(|v| v.inner.borrow_mut())
                        .collect::<Vec<_>>();
                    let children = children_ref
                        .iter_mut()
                        .filter_map(|v| v.render_object.as_mut())
                        .collect();
                    layout_engine.borrow_mut().finalize_layout(render, children);
                }
                render.layout_engine = layout_engine;
            }
            if let Some(parent) = inner.parent.as_ref().and_then(|v| v.upgrade()) {
                let parent = parent.borrow();
                layout.post_position_child(
                    inner.render_object.as_mut().unwrap(),
                    parent.render_object.as_ref().unwrap(),
                );
            }
        }
    }

    fn render<V>(&self, styles: &Styles<RInfo>, visitor: &mut V)
    where
        V: RenderVisitor<RInfo>,
    {
        {
            let mut inner = self.inner.borrow_mut();
            if let Some(render) = inner.render_object.as_mut() {
                visitor.visit(render);
            }
        }
        {
            let inner = self.inner.borrow();
            if let NodeValue::Element(ref e) = inner.value {
                for c in &e.children {
                    c.render(styles, visitor);
                }
            }
        }

        let mut inner = self.inner.borrow_mut();
        if let Some(render) = inner.render_object.as_mut() {
            visitor.visit_end(render);
        }
    }

    /// Creates a new element with the given name.
    pub fn new<S>(name: S) -> Node<RInfo>
    where
        S: Into<String>,
    {
        Node {
            inner: Rc::new(RefCell::new(NodeInner {
                parent: None,
                value: NodeValue::Element(Element {
                    name: name.into(),
                    children: Vec::new(),
                }),
                properties: FnvHashMap::default(),
                render_object: None,
                dirty: true,
            })),
        }
    }

    /// Creates a new text node with the given text.
    pub fn new_text<S>(text: S) -> Node<RInfo>
    where
        S: Into<String>,
    {
        Node {
            inner: Rc::new(RefCell::new(NodeInner {
                parent: None,
                value: NodeValue::Text(text.into()),
                properties: FnvHashMap::default(),
                render_object: None,
                dirty: true,
            })),
        }
    }
    /// Adds the passed node as a child to this node
    /// before other child nodes.
    ///
    /// This panics if the passed node already has a parent
    /// or if the node is a text node.
    pub fn add_child_first(&self, node: Node<RInfo>) {
        assert!(
            node.inner.borrow().parent.is_none(),
            "Node already has a parent"
        );
        if let NodeValue::Element(ref mut e) = self.inner.borrow_mut().value {
            node.inner.borrow_mut().parent = Some(Rc::downgrade(&self.inner));
            e.children.insert(0, node);
        } else {
            panic!("Text cannot have child elements")
        }
    }

    /// Adds the passed node as a child to this node.
    ///
    /// This panics if the passed node already has a parent
    /// or if the node is a text node.
    pub fn add_child(&self, node: Node<RInfo>) {
        assert!(
            node.inner.borrow().parent.is_none(),
            "Node already has a parent"
        );
        if let NodeValue::Element(ref mut e) = self.inner.borrow_mut().value {
            node.inner.borrow_mut().parent = Some(Rc::downgrade(&self.inner));
            e.children.push(node);
        } else {
            panic!("Text cannot have child elements")
        }
    }

    /// Removes the passed node as a child from this node.
    ///
    /// This panics if the passed node 's parent isn't this node
    /// or if the node is a text node.
    pub fn remove_child(&self, node: Node<RInfo>) {
        assert!(
            node.inner
                .borrow()
                .parent
                .as_ref()
                .and_then(|v| v.upgrade())
                .map_or(false, |v| Rc::ptr_eq(&v, &self.inner)),
            "Node isn't child to this element"
        );
        let inner: &mut NodeInner<_> = &mut *self.inner.borrow_mut();
        if let NodeValue::Element(ref mut e) = inner.value {
            e.children.retain(|v| !Rc::ptr_eq(&v.inner, &node.inner));
            inner.dirty = true;
        } else {
            panic!("Text cannot have child elements")
        }
    }

    /// Returns a vector containing the child nodes of this
    /// node.
    pub fn children(&self) -> Vec<Node<RInfo>> {
        if let NodeValue::Element(ref e) = self.inner.borrow().value {
            Clone::clone(&e.children)
        } else {
            Vec::new()
        }
    }

    /// Returns the parent node of this node.
    ///
    /// This panics if the node doesn't have a parent.
    /// A node only doesn't have a parent before its
    /// added to another node or if its the root node.
    pub fn parent(&self) -> Node<RInfo> {
        let inner = self.inner.borrow();
        inner
            .parent
            .as_ref()
            .and_then(|v| v.upgrade())
            .map(|v| Node { inner: v })
            .expect("Node hasn't got a parent")
    }

    /// Returns the name of the node if it has one
    pub fn name(&self) -> Option<String> {
        let inner = self.inner.borrow();
        match inner.value {
            NodeValue::Element(ref e) => Some(e.name.clone()),
            NodeValue::Text(_) => None,
        }
    }

    /// Returns whether the passed node points to the same node
    /// as this one
    pub fn is_same(&self, other: &Node<RInfo>) -> bool {
        Rc::ptr_eq(&self.inner, &other.inner)
    }

    /// Returns the text of the node if it is a text node.
    pub fn text(&self) -> Option<String> {
        if let NodeValue::Text(ref t) = self.inner.borrow().value {
            Some(t.clone())
        } else {
            None
        }
    }

    /// Sets the text of the node if it is a text node.
    pub fn set_text<S>(&self, txt: S)
    where
        S: Into<String>,
    {
        let inner: &mut NodeInner<_> = &mut *self.inner.borrow_mut();
        if let NodeValue::Text(ref mut t) = inner.value {
            *t = txt.into();
            inner.dirty = true;
        }
    }

    /// Returns the `RenderObject` for this node.
    ///
    /// Must be called after a `layout` call
    pub fn render_object(&self) -> Ref<RenderObject<RInfo>> {
        let inner = self.inner.borrow();
        Ref::map(inner, |v| v.render_object.as_ref().unwrap())
    }

    /// Returns the raw position of the node.
    ///
    /// This position isn't transformed and is relative
    /// to the parent instead of absolute like `render_position`
    pub fn raw_position(&self) -> Rect {
        let inner = self.inner.borrow();
        inner
            .render_object
            .as_ref()
            .map(|v| v.draw_rect)
            .unwrap_or(Rect {
                x: 0,
                y: 0,
                width: 0,
                height: 0,
            })
    }

    /// Returns the rendering position of the node.
    ///
    /// Useful for IME handling.
    /// Must be called after a `layout` call.
    pub fn render_position(&self) -> Option<Rect> {
        let inner = self.inner.borrow();
        let mut rect = match inner.render_object.as_ref() {
            Some(v) => v.draw_rect,
            None => return None,
        };
        let mut cur = inner.parent.as_ref().and_then(|v| v.upgrade());
        while let Some(p) = cur {
            let inner = p.borrow();
            let p_obj = match inner.render_object.as_ref() {
                Some(v) => v,
                None => return None,
            };
            rect.x += p_obj.scroll_position.0 as i32;
            rect.y += p_obj.scroll_position.1 as i32;
            if p_obj.clip_overflow {
                if rect.x < 0 {
                    rect.width += rect.x;
                    rect.x = 0;
                }
                if rect.y < 0 {
                    rect.height += rect.y;
                    rect.y = 0;
                }
                if rect.x + rect.width >= p_obj.draw_rect.width {
                    rect.width -= (rect.x + rect.width) - p_obj.draw_rect.width;
                }
                if rect.y + rect.height >= p_obj.draw_rect.height {
                    rect.height -= (rect.y + rect.height) - p_obj.draw_rect.height;
                }
            }
            if rect.width <= 0 || rect.height <= 0 {
                return None;
            }

            rect.x += p_obj.draw_rect.x;
            rect.y += p_obj.draw_rect.y;
            cur = inner.parent.as_ref().and_then(|v| v.upgrade());
        }
        Some(rect)
    }

    /// Returns the value of the property if it has it set
    pub fn get_property<V: PropertyValue>(&self, key: &str) -> Option<V> {
        let inner = self.inner.borrow();
        inner.properties.get(key).and_then(|v| V::convert_from(&v))
    }

    /// Gets the custom value from the proprety for this node
    pub fn get_custom_property<V: Clone + CustomValue + 'static>(&self, name: &str) -> Option<V> {
        let inner = self.inner.borrow();
        inner.properties.get(name).and_then(|v| {
            if let Value::Any(ref v) = *v {
                (**v).as_any().downcast_ref::<V>().cloned()
            } else {
                None
            }
        })
    }

    /// Sets the value of the property on the node.
    pub fn set_property<V: PropertyValue>(&self, key: &str, value: V) {
        let mut inner = self.inner.borrow_mut();
        inner.dirty = true;
        inner.properties.insert(key.into(), value.convert_into());
    }

    /// Sets the value of the property on the node without
    /// flagging it as dirty
    pub fn raw_set_property<V: PropertyValue>(&self, key: &str, value: V) {
        let mut inner = self.inner.borrow_mut();
        inner.properties.insert(key.into(), value.convert_into());
    }

    /// Removes the property on the node.
    pub fn remove_property(&self, key: &str) {
        let mut inner = self.inner.borrow_mut();
        inner.dirty = true;
        inner.properties.remove(key);
    }

    /// Returns whether the object has had its layout computed at
    /// least once.
    pub fn has_layout(&self) -> bool {
        let inner = self.inner.borrow();
        inner.render_object.is_some()
    }

    /// Gets the value from the style rules for this node
    pub fn get_value<V: PropertyValue>(&self, name: &str) -> Option<V> {
        let inner = self.inner.borrow();
        inner.render_object.as_ref().and_then(|v| v.get_value(name))
    }

    /// Gets the custom value from the style rules for this node
    pub fn get_custom_value<V: Clone + CustomValue + 'static>(&self, name: &str) -> Option<V> {
        let inner = self.inner.borrow();
        inner
            .render_object
            .as_ref()
            .and_then(|v| v.get_custom_value(name))
            .map(|v| Clone::clone(v))
    }

    /// Begins a query on this node
    pub fn query(&self) -> query::Query<RInfo> {
        query::Query::new(self.clone())
    }

    /// Creates a weak reference to this node.
    pub fn weak(&self) -> WeakNode<RInfo> {
        WeakNode {
            inner: Rc::downgrade(&self.inner),
        }
    }

    /// Creates a node from a string
    pub fn from_str(s: &str) -> Result<Node<RInfo>, syntax::PError> {
        syntax::desc::Document::parse(s).map(|v| Node::from_document(v))
    }

    /// Creates a node from a parsed document.
    pub fn from_document(desc: syntax::desc::Document) -> Node<RInfo> {
        Node::from_doc_element(desc.root)
    }

    fn from_doc_text(
        desc: String,
        properties: FnvHashMap<syntax::Ident, syntax::desc::ValueType>,
    ) -> Node<RInfo> {
        Node {
            inner: Rc::new(RefCell::new(NodeInner {
                parent: None,
                value: NodeValue::Text(desc),
                properties: properties
                    .into_iter()
                    .map(|(n, v)| (n.name, v.into()))
                    .collect(),
                render_object: None,
                dirty: true,
            })),
        }
    }

    fn from_doc_element(desc: syntax::desc::Element) -> Node<RInfo> {
        let node = Node {
            inner: Rc::new(RefCell::new(NodeInner {
                parent: None,
                value: NodeValue::Element(Element {
                    name: desc.name.name,
                    children: Vec::with_capacity(desc.nodes.len()),
                }),
                properties: desc.properties
                    .into_iter()
                    .map(|(n, v)| (n.name, v.into()))
                    .collect(),
                render_object: None,
                dirty: true,
            })),
        };

        for c in desc.nodes.into_iter().map(|n| match n {
            syntax::desc::Node::Element(e) => Node::from_doc_element(e),
            syntax::desc::Node::Text(t, _, props) => Node::from_doc_text(t, props),
        }) {
            node.add_child(c);
        }

        node
    }

    fn root() -> Node<RInfo> {
        Node {
            inner: Rc::new(RefCell::new(NodeInner {
                parent: None,
                value: NodeValue::Element(Element {
                    name: "root".into(),
                    children: Vec::new(),
                }),
                properties: FnvHashMap::default(),
                render_object: Some(RenderObject::default()),
                dirty: false,
            })),
        }
    }
}

/// A weak reference to a node.
pub struct WeakNode<RInfo> {
    inner: Weak<RefCell<NodeInner<RInfo>>>,
}
impl<RInfo> WeakNode<RInfo> {
    /// Tries to upgrade this weak reference into a strong one.
    ///
    /// Fails if there isn't any strong references to the node.
    pub fn upgrade(&self) -> Option<Node<RInfo>> {
        self.inner.upgrade().map(|v| Node { inner: v })
    }
}

impl<RInfo> Clone for WeakNode<RInfo> {
    fn clone(&self) -> Self {
        WeakNode {
            inner: self.inner.clone(),
        }
    }
}

struct NodeInner<RInfo> {
    parent: Option<Weak<RefCell<NodeInner<RInfo>>>>,
    properties: FnvHashMap<String, Value>,
    value: NodeValue<RInfo>,
    render_object: Option<RenderObject<RInfo>>,
    dirty: bool,
}

enum NodeValue<RInfo> {
    Element(Element<RInfo>),
    Text(String),
}

struct Element<RInfo> {
    name: String,
    children: Vec<Node<RInfo>>,
}

/// A value that can be used as a style attribute
#[derive(Debug)]
pub enum Value {
    Boolean(bool),
    Integer(i32),
    Float(f64),
    String(String),
    Any(Box<CustomValue>),
}

impl Value {
    /// Tries to convert this value into the type.
    pub fn get_value<V: PropertyValue>(&self) -> Option<V> {
        V::convert_from(self)
    }

    /// Tries to convert this value into the custom type.
    pub fn get_custom_value<V: CustomValue + 'static>(&self) -> Option<&V> {
        if let Value::Any(ref v) = *self {
            (**v).as_any().downcast_ref::<V>()
        } else {
            None
        }
    }
}

impl Clone for Value {
    fn clone(&self) -> Value {
        match *self {
            Value::Boolean(v) => Value::Boolean(v),
            Value::Integer(v) => Value::Integer(v),
            Value::Float(v) => Value::Float(v),
            Value::String(ref v) => Value::String(v.clone()),
            Value::Any(ref v) => Value::Any((*v).clone()),
        }
    }
}

impl PartialEq for Value {
    fn eq(&self, rhs: &Value) -> bool {
        use Value::*;
        match (self, rhs) {
            (&Boolean(a), &Boolean(b)) => a == b,
            (&Integer(a), &Integer(b)) => a == b,
            (&Float(a), &Float(b)) => a == b,
            (&String(ref a), &String(ref b)) => a == b,
            _ => false,
        }
    }
}

impl From<syntax::desc::ValueType> for Value {
    fn from(v: syntax::desc::ValueType) -> Value {
        match v.value {
            syntax::desc::Value::Boolean(val) => Value::Boolean(val),
            syntax::desc::Value::Integer(val) => Value::Integer(val),
            syntax::desc::Value::Float(val) => Value::Float(val),
            syntax::desc::Value::String(val) => Value::String(val),
        }
    }
}

/// The value passed to layout engines and render visitors
/// in order to render the nodes.
///
/// `render_info` is used by the renderer and not stylish.
pub struct RenderObject<RInfo> {
    /// The position and size of the element
    /// as decided by the layout engine.
    pub draw_rect: Rect,
    /// The smallest this object can be
    pub min_size: (i32, i32),
    /// The largest this object can be.
    ///
    /// None for no limit
    pub max_size: (Option<i32>, Option<i32>),
    layout_engine: RefCell<Box<LayoutEngine<RInfo>>>,
    vars: FnvHashMap<String, Value>,
    /// Renderer storage
    pub render_info: Option<RInfo>,
    /// The text of this element if it is text.
    pub text: Option<String>,
    pub text_splits: Vec<(usize, usize, Rect)>,

    /// Scroll offset position
    pub scroll_position: (f64, f64),
    /// Whether to clip elements that fall outside this
    /// element
    pub clip_overflow: bool,
}

impl<RInfo> RenderObject<RInfo> {
    /// Gets the value from the style rules for this element
    pub fn get_value<V: PropertyValue>(&self, name: &str) -> Option<V> {
        match name {
            "scroll_x" => V::convert_from(&Value::Float(self.scroll_position.0)),
            "scroll_y" => V::convert_from(&Value::Float(self.scroll_position.1)),
            "clip_overflow" => V::convert_from(&Value::Boolean(self.clip_overflow)),
            _ => self.vars.get(name).and_then(|v| V::convert_from(&v)),
        }
    }

    /// Gets the custom value from the style rules for this element
    pub fn get_custom_value<V: CustomValue + 'static>(&self, name: &str) -> Option<&V> {
        self.vars.get(name).and_then(|v| {
            if let Value::Any(ref v) = *v {
                (**v).as_any().downcast_ref::<V>()
            } else {
                None
            }
        })
    }
}

impl<RInfo> Default for RenderObject<RInfo> {
    fn default() -> RenderObject<RInfo> {
        RenderObject {
            draw_rect: Rect {
                x: 0,
                y: 0,
                width: 0,
                height: 0,
            },
            min_size: (0, 0),
            max_size: (None, None),
            layout_engine: RefCell::new(Box::new(AbsoluteLayout)),
            vars: FnvHashMap::default(),
            render_info: Default::default(),
            text: None,
            text_splits: Vec::new(),
            scroll_position: (0.0, 0.0),
            clip_overflow: false,
        }
    }
}

/// A value that can be stored as a property
pub trait PropertyValue: Sized {
    /// Converts a value into this type
    fn convert_from(v: &Value) -> Option<Self>;
    /// Converts this type into a value
    fn convert_into(self) -> Value;
}

/// A type that can be converted into `Any`
pub trait Anyable: Any {
    /// Converts this type to `Any`
    fn as_any(&self) -> &Any;
}

impl<T: Any> Anyable for T {
    fn as_any(&self) -> &Any {
        self
    }
}

/// A non-standard type that can be used as a property
/// value.
pub trait CustomValue: Anyable {
    /// Clones this type
    fn clone(&self) -> Box<CustomValue>;
}

impl ::std::fmt::Debug for Box<CustomValue> {
    fn fmt(&self, f: &mut ::std::fmt::Formatter) -> ::std::fmt::Result {
        write!(f, "CustomValue")
    }
}

impl<T: Clone + 'static> CustomValue for Vec<T> {
    fn clone(&self) -> Box<CustomValue> {
        Box::new(Clone::clone(self))
    }
}

impl<T: CustomValue + 'static> PropertyValue for T {
    fn convert_from(_v: &Value) -> Option<Self> {
        panic!("Can't convert into T")
    }
    fn convert_into(self) -> Value {
        Value::Any(Box::new(self))
    }
}

impl PropertyValue for Value {
    fn convert_from(v: &Value) -> Option<Self> {
        Some(v.clone())
    }
    fn convert_into(self) -> Value {
        self
    }
}

impl PropertyValue for bool {
    fn convert_from(v: &Value) -> Option<Self> {
        match *v {
            Value::Boolean(v) => Some(v),
            _ => None,
        }
    }

    fn convert_into(self) -> Value {
        Value::Boolean(self)
    }
}

impl PropertyValue for i32 {
    fn convert_from(v: &Value) -> Option<Self> {
        match *v {
            Value::Integer(v) => Some(v),
            Value::Float(v) => Some(v as i32),
            _ => None,
        }
    }

    fn convert_into(self) -> Value {
        Value::Integer(self)
    }
}

impl PropertyValue for f64 {
    fn convert_from(v: &Value) -> Option<Self> {
        match *v {
            Value::Integer(v) => Some(v as f64),
            Value::Float(v) => Some(v),
            _ => None,
        }
    }

    fn convert_into(self) -> Value {
        Value::Float(self)
    }
}

impl PropertyValue for String {
    fn convert_from(v: &Value) -> Option<Self> {
        match *v {
            Value::String(ref v) => Some(v.clone()),
            _ => None,
        }
    }

    fn convert_into(self) -> Value {
        Value::String(self)
    }
}
