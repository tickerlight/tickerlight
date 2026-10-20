//! Controls around a chart, drawn into the same scene as the chart itself.
//!
//! [`Ui`] is an immediate-mode widget kit: a panel is a function that lays its widgets out from the current
//! state every time, once to react to an input event and once to draw. [`Workspace`] is a window holding the
//! chart and the panels docked around it; it routes the input to whichever is under the pointer.

pub mod icons;
mod indicators;
pub mod panels;
mod strategies;
mod theme;
mod ui;
mod workspace;

pub use icons::Icon;
pub use indicators::{IndicatorsPanel, SEARCH_INDICATORS};
pub use strategies::{StrategyFiles, StrategyPanel};
pub use theme::Theme;
pub use ui::{Area, Cell, Column, Sort, TableResponse, TextResponse, Ui, UiState};
pub use workspace::{Dock, Layout, Panel, Workspace};
