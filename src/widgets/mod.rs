use crossterm::event::KeyEvent;

pub mod error;
pub mod progress;
pub mod rooms;
pub mod signin;

pub mod button;
pub mod chat;
pub mod textinput;

pub trait KeyEventing {
    // returns true if the event was consumed
    fn input(&mut self, input: &KeyEvent) -> bool;
}

fn send(mut elements: Vec<Box<dyn KeyEventing + '_>>, event: &KeyEvent) -> bool {
    for e in elements.iter_mut() {
        if e.input(event) {
            return true;
        }
    }

    false
}

pub trait Focusable {
    fn focused(&self) -> bool;
    fn focus(&mut self);
    fn defocus(&mut self);
}

// given a list of elements, defocus the current one and focus the next one
fn focus_next(mut elements: Vec<Box<dyn Focusable + '_>>) {
    if elements.is_empty() {
        return;
    }

    let mut next: bool = false;

    for e in elements.iter_mut() {
        if next {
            e.focus();
            return;
        }

        if e.focused() {
            e.defocus();
            next = true;
        }
    }

    // either nothing or the last element was focused
    elements[0].focus()
}

fn focus_prev<'a>(mut elements: Vec<Box<dyn Focusable + 'a>>) {
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
