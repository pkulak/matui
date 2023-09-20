use crate::app::App;
use crate::widgets::EventResult::Ignored;

pub mod error;
pub mod progress;
pub mod rooms;
pub mod signin;
pub mod help;

pub mod button;
pub mod chat;
pub mod confirm;
pub mod message;
pub mod react;
pub mod receipts;
pub mod textinput;

#[macro_export]
macro_rules! consumed {
    () => {
        $crate::widgets::EventResult::Consumed(Box::new(|_| ()))
    };
}

#[macro_export]
macro_rules! close {
    () => {
        $crate::widgets::EventResult::Consumed(Box::new(|app| app.close_popup()))
    };
}

pub enum EventResult {
    // The widget has chosen to "consume" the event, modifying its state
    // as needed. The function is the widget's opportunity to modify
    // the state of the App itself.
    Consumed(Box<dyn FnOnce(&mut App)>),

    /// The widget has chosen to "ignore" the event; it will be passed along
    /// to any other subling widgets.
    Ignored,
}

pub trait Focusable {
    fn focused(&self) -> bool;
    fn focus(&mut self);
    fn defocus(&mut self);
}

// given a list of elements, defocus the current one and focus the next one
fn focus_next(mut elements: Vec<Box<dyn Focusable + '_>>) -> EventResult {
    if elements.is_empty() {
        return Ignored;
    }

    let mut next: bool = false;

    for e in elements.iter_mut() {
        if next {
            e.focus();
            return consumed!();
        }

        if e.focused() {
            e.defocus();
            next = true;
        }
    }

    // either nothing or the last element was focused
    elements[0].focus();

    consumed!()
}

fn focus_prev<'a>(mut elements: Vec<Box<dyn Focusable + 'a>>) -> EventResult {
    elements.reverse();
    focus_next(elements)
}

fn get_margin(available: u16, requested: u16) -> u16 {
    if requested >= available {
        0
    } else {
        (available - requested) / 2
    }
}
