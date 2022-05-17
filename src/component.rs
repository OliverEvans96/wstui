use async_trait::async_trait;
use tui::{layout::Rect, widgets::Widget};

// TODO: Just use StatefulWidget?
#[async_trait]
pub trait Component {
    type Action;

    // TODO: Update immutably?
    /// Update state (mutably) from on an action.
    async fn update(&mut self, action: Self::Action);

    /// Create the widget view from the state.
    async fn render(&self, area: &Rect) -> Box<dyn Widget>;
}
