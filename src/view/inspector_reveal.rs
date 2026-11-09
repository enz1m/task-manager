//! Owns the inspector frame clock; PanelReveal owns layout, paint, and input boundaries.

use enzui_widgets::motion::{
    PanelMotion, PanelReveal, PanelRevealClock, PanelRevealEdge, PanelRevealMode,
};
use iced::advanced::{Layout, Shell, Widget, layout, mouse, overlay, renderer, widget};
use iced::{Element, Event, Length, Rectangle, Size, Vector};

use crate::message::Message;

pub(super) fn reveal<'a>(
    child: impl Into<Element<'a, Message>>,
    visible: bool,
    vertical: bool,
    extent: f32,
    theme: &enzui_theme::EnzTheme,
) -> Element<'a, Message> {
    let clock = PanelRevealClock::new().layout_driven(true);
    let motion = PanelMotion::at_target(visible);
    motion.observe_theme(&theme.iced_theme());
    clock.advance(&motion, iced::time::Instant::now());
    let panel = if vertical {
        PanelReveal::vertical(
            child.into(),
            clock.progress(),
            Some(extent),
            PanelRevealMode::Reserve,
        )
    } else {
        PanelReveal::new(
            child.into(),
            clock.progress(),
            Some(extent),
            PanelRevealMode::Reserve,
        )
        .enters_from(if crate::i18n::rtl() {
            PanelRevealEdge::Leading
        } else {
            PanelRevealEdge::Trailing
        })
    };
    Element::new(InspectorReveal {
        child: panel.clock(&clock).into(),
        clock,
        visible,
        theme: theme.iced_theme(),
    })
}

struct InspectorReveal<'a> {
    child: Element<'a, Message>,
    visible: bool,
    clock: PanelRevealClock,
    theme: iced::Theme,
}

impl InspectorReveal<'_> {
    fn child_layout(layout: Layout, tree: &widget::Tree) -> Layout {
        layout
            .children(&tree.children)
            .next()
            .expect("inspector child layout")
    }
}

impl Widget<Message, iced::Theme, iced::Renderer> for InspectorReveal<'_> {
    fn tag(&self) -> widget::tree::Tag {
        widget::tree::Tag::of::<PanelMotion>()
    }

    fn state(&self) -> widget::tree::State {
        let motion = PanelMotion::at_target(self.visible);
        motion.observe_theme(&self.theme);
        widget::tree::State::new(motion)
    }

    fn diff(&mut self, tree: &mut widget::Tree) {
        let motion = tree.state.downcast_mut::<PanelMotion>();
        motion.observe_theme(&self.theme);
        motion.set_target_at_frame(self.visible);
        self.clock.advance(
            motion,
            motion.frame_at().unwrap_or_else(iced::time::Instant::now),
        );
        tree.diff_children(std::slice::from_mut(&mut self.child));
    }

    fn frame_interest(&self, tree: &widget::Tree) -> bool {
        let motion = tree.state.downcast_ref::<PanelMotion>();
        motion.queued_from().is_some()
            || motion.sampled_animating()
            || tree.children.iter().any(widget::Tree::has_frame_interest)
    }

    fn size(&self) -> Size<Length> {
        self.child.as_widget().size()
    }

    fn layout(
        &mut self,
        tree: &mut widget::Tree,
        renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) {
        use iced::advanced::layout::Measure as _;
        let child = self
            .child
            .as_widget_mut()
            .measure(&mut tree.children[0], renderer, limits);
        layout::Node::with_children(child.size(), vec![child]).apply(tree);
        tree.state
            .downcast_mut::<PanelMotion>()
            .note_layout_at_frame();
    }

    fn update(
        &mut self,
        tree: &mut widget::Tree,
        event: &Event,
        layout: Layout,
        cursor: mouse::Cursor,
        renderer: &iced::Renderer,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        if let Event::Window(iced::window::Event::RedrawRequested(at)) = event {
            let motion = tree.state.downcast_mut::<PanelMotion>();
            motion.note_frame(*at);
            self.clock.advance(motion, *at);
            if motion.sampled_needs_layout() {
                shell.invalidate_layout();
            }
            if motion.sampled_animating() {
                shell.request_redraw();
            }
        }
        let child_layout = Self::child_layout(layout, tree);
        self.child.as_widget_mut().update(
            &mut tree.children[0],
            event,
            child_layout,
            cursor,
            renderer,
            shell,
            viewport,
        );
    }

    fn draw(
        &self,
        tree: &widget::Tree,
        renderer: &mut iced::Renderer,
        theme: &iced::Theme,
        style: &renderer::Style,
        layout: Layout,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        self.child.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            Self::child_layout(layout, tree),
            cursor,
            viewport,
        );
    }

    fn operate(
        &mut self,
        tree: &mut widget::Tree,
        layout: Layout,
        viewport: &Rectangle,
        renderer: &iced::Renderer,
        operation: &mut dyn widget::Operation,
    ) {
        let child_layout = Self::child_layout(layout, tree);
        self.child.as_widget_mut().operate(
            &mut tree.children[0],
            child_layout,
            viewport,
            renderer,
            operation,
        );
    }

    fn mouse_interaction(
        &self,
        tree: &widget::Tree,
        layout: Layout,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        self.child.as_widget().mouse_interaction(
            &tree.children[0],
            Self::child_layout(layout, tree),
            cursor,
            viewport,
            renderer,
        )
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut widget::Tree,
        layout: Layout,
        renderer: &iced::Renderer,
        viewport: &Rectangle,
        translation: Vector,
        window: Size,
    ) -> Vec<overlay::Element<'b, Message, iced::Theme, iced::Renderer>> {
        let child_layout = Self::child_layout(layout, tree);
        self.child.as_widget_mut().overlay(
            &mut tree.children[0],
            child_layout,
            renderer,
            viewport,
            translation,
            window,
        )
    }
}
