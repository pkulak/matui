use crate::widgets::Action::ChangeFocus;
use crate::widgets::EventResult::{Consumed, Ignored};
use crossterm::event::KeyEvent;

pub mod error;
pub mod progress;
pub mod rooms;
pub mod signin;

pub mod button;
pub mod chat;
pub mod confirm;
pub mod textinput;

pub enum EventResult {
    Consumed(Action),
    Ignored,
}

pub enum Action {
    ButtonYes,
    ButtonNo,
    Typing,
    ChangeFocus,
}

pub trait KeyEventing {
    fn input(&mut self, input: &KeyEvent) -> EventResult;
}

fn send(mut elements: Vec<Box<dyn KeyEventing + '_>>, event: &KeyEvent) -> EventResult {
    for e in elements.iter_mut() {
        if let Consumed(e) = e.input(event) {
            return Consumed(e);
        }
    }

    Ignored
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
            return Consumed(ChangeFocus);
        }

        if e.focused() {
            e.defocus();
            next = true;
        }
    }

    // either nothing or the last element was focused
    elements[0].focus();

    Consumed(ChangeFocus)
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
