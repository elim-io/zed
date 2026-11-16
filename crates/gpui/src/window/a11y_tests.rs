use crate::{
    self as gpui, AnyView, AppContext as _, Bounds, ClickEvent, Context, Entity,
    InteractiveElement, IntoElement, KeyboardButton, ParentElement, Render,
    StatefulInteractiveElement, StyleRefinement, Styled, TestAppContext, VisualTestContext, Window,
    div, point, px, size,
};
use accesskit::{Action, ActionRequest, Node, NodeId, Role, Toggled, TreeId};
use std::{cell::RefCell, rc::Rc};

#[derive(Default)]
struct Pressed {
    save: Vec<ClickEvent>,
    delete: usize,
}

struct Controls {
    pressed: Rc<RefCell<Pressed>>,
}

impl Render for Controls {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let save = self.pressed.clone();
        let delete = self.pressed.clone();
        div()
            .id("controls")
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .id("save")
                    .role(Role::Button)
                    .aria_label("Save")
                    .aria_class_name("Button")
                    .w(px(100.))
                    .h(px(20.))
                    .on_click(move |event, _, _| save.borrow_mut().save.push(event.clone())),
            )
            .child(
                div()
                    .id("wifi")
                    .role(Role::Switch)
                    .aria_label("Wi-Fi")
                    .aria_class_name("Switch")
                    .aria_toggled(Toggled::True)
                    .h(px(20.))
                    .on_click(|_, _, _| {}),
            )
            .child(
                div()
                    .id("delete")
                    .role(Role::Button)
                    .aria_label("Delete")
                    .aria_disabled(true)
                    .h(px(20.))
                    .on_click(move |_, _, _| delete.borrow_mut().delete += 1),
            )
            .child(
                div()
                    .id("name")
                    .role(Role::TextInput)
                    .aria_label("Name")
                    .aria_value("Ada")
                    .aria_placeholder("Your name")
                    .aria_description("Shown to others")
                    .h(px(20.)),
            )
            .child(
                div()
                    .id("confirm")
                    .role(Role::AlertDialog)
                    .aria_label("Discard changes")
                    .aria_modal(true)
                    .h(px(20.)),
            )
    }
}

fn node<'a>(window: &'a Window, identifier: &str) -> Option<(NodeId, &'a Node)> {
    window
        .a11y_tree()?
        .nodes
        .iter()
        .find(|(_, node)| node.author_id() == Some(identifier))
        .map(|(id, node)| (*id, node))
}

fn press(cx: &mut VisualTestContext, identifier: &str) {
    cx.update(|window, cx| {
        let (target_node, _) = node(window, identifier).expect("the node is in the tree");
        window.a11y_perform(
            ActionRequest {
                action: Action::Click,
                target_tree: TreeId::ROOT,
                target_node,
                data: None,
            },
            cx,
        );
    });
}

#[gpui::test]
fn the_tree_holds_the_setters_and_a_press_calls_the_handler(cx: &mut TestAppContext) {
    let pressed = Rc::new(RefCell::new(Pressed::default()));
    let (_, cx) = cx.add_window_view({
        let pressed = pressed.clone();
        |_, _| Controls { pressed }
    });

    // No AX client: no tree is built.
    cx.update(|window, _| assert!(window.a11y_tree().is_none()));

    cx.update(|window, _| window.a11y_force_active());
    cx.run_until_parked();

    cx.update(|window, _| {
        assert!(window.a11y_duplicates().is_empty());

        let (_, save) = node(window, "save").expect("save");
        assert_eq!(save.role(), Role::Button);
        assert_eq!(save.label(), Some("Save"));
        assert_eq!(save.class_name(), Some("Button"));
        assert!(save.supports_action(Action::Click));
        assert!(!save.is_disabled());

        let (_, wifi) = node(window, "wifi").expect("wifi");
        assert_eq!(wifi.role(), Role::Switch);
        assert_eq!(wifi.toggled(), Some(Toggled::True));

        let (_, delete) = node(window, "delete").expect("delete");
        assert!(delete.is_disabled());
        assert!(!delete.supports_action(Action::Click));

        let (_, name) = node(window, "name").expect("name");
        assert_eq!(name.role(), Role::TextInput);
        assert_eq!(name.value(), Some("Ada"));
        assert_eq!(name.placeholder(), Some("Your name"));
        assert_eq!(name.description(), Some("Shown to others"));

        let (_, confirm) = node(window, "confirm").expect("confirm");
        assert!(confirm.is_modal());

        // The container has an id and no role: no node, no identifier.
        assert!(node(window, "controls").is_none());
    });

    press(cx, "save");
    {
        let pressed = pressed.borrow();
        assert_eq!(pressed.save.len(), 1);
        let ClickEvent::Keyboard(event) = &pressed.save[0] else {
            panic!("the press is a keyboard click, not a synthesized mouse");
        };
        assert_eq!(event.button, KeyboardButton::Enter);
        assert_eq!(
            event.bounds,
            Bounds::new(point(px(0.), px(0.)), size(px(100.), px(20.)))
        );
    }

    press(cx, "delete");
    assert_eq!(
        pressed.borrow().delete,
        0,
        "a disabled control takes no press"
    );
}

struct Cached {
    renders: usize,
    child: Entity<CachedChild>,
}

struct CachedChild;

impl Render for CachedChild {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("cached-label")
            .role(Role::Label)
            .aria_label("Cached")
            .size_full()
    }
}

impl Render for Cached {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.renders += 1;
        div()
            .id("outer")
            .size_full()
            .child(
                div()
                    .id("renders")
                    .role(Role::Label)
                    .aria_label(format!("renders {}", self.renders))
                    .h(px(20.)),
            )
            .child(AnyView::from(self.child.clone()).cached(StyleRefinement::default().size_full()))
    }
}

fn label(window: &Window, identifier: &str) -> Option<String> {
    node(window, identifier).and_then(|(_, node)| node.label().map(str::to_string))
}

#[gpui::test]
fn a_cached_view_keeps_its_nodes(cx: &mut TestAppContext) {
    let (view, cx) = cx.add_window_view(|_, cx| Cached {
        renders: 0,
        child: cx.new(|_| CachedChild),
    });
    cx.update(|window, _| window.a11y_force_active());
    cx.run_until_parked();
    let first = cx.update(|window, _| {
        assert!(node(window, "cached-label").is_some());
        label(window, "renders")
    });

    // The outer view redraws, the cached child is clean: a frame that would
    // reuse the child's prepaint, and with it lose the child's nodes.
    view.update(cx, |_, cx| cx.notify());
    cx.run_until_parked();
    cx.update(|window, _| {
        assert_ne!(label(window, "renders"), first, "a second frame was drawn");
        assert_eq!(label(window, "cached-label").as_deref(), Some("Cached"));
    });
}

struct Twins;

impl Render for Twins {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("twins")
            .size_full()
            .child(
                div()
                    .id("twin")
                    .role(Role::Button)
                    .aria_label("First")
                    .h(px(20.)),
            )
            .child(
                div()
                    .id("twin")
                    .role(Role::Button)
                    .aria_label("Second")
                    .h(px(20.)),
            )
    }
}

#[gpui::test]
fn a_duplicate_id_is_reported_not_panicked_on(cx: &mut TestAppContext) {
    let (_, cx) = cx.add_window_view(|_, _| Twins);
    cx.update(|window, _| window.a11y_force_active());
    cx.run_until_parked();
    cx.update(|window, _| {
        assert_eq!(window.a11y_duplicates().len(), 1);
        assert!(window.a11y_duplicates()[0].ends_with("twins.twin"));
        assert_eq!(label(window, "twin").as_deref(), Some("First"));
    });
}
