//! Feeds winit events into Dear ImGui.
//!
//! ImGui keeps its own copy of the pointer, the keyboard modifiers and the set
//! of keys currently down, and expects to be told about every change. Nothing
//! here interprets the events; that is the UI's job.

use imgui::{Io, Key};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{KeyCode, NamedKey, PhysicalKey};
use winit::window::Window;

/// Applies one window event. Returns whether the UI wants to keep the event to
/// itself -- true while the pointer is over a window or a text field has focus,
/// in which case the emulated machine should not also see it.
pub fn handle_event(io: &mut Io, window: &Window, event: &WindowEvent) -> bool {
    match event {
        WindowEvent::Resized(size) => {
            io.display_size = [size.width as f32, size.height as f32];
            io.display_framebuffer_scale = [1.0, 1.0];
            false
        }
        WindowEvent::ScaleFactorChanged { .. } => {
            let size = window.inner_size();
            io.display_size = [size.width as f32, size.height as f32];
            false
        }
        WindowEvent::CursorMoved { position, .. } => {
            io.add_mouse_pos_event([position.x as f32, position.y as f32]);
            io.want_capture_mouse
        }
        WindowEvent::MouseInput { state, button, .. } => {
            let pressed = *state == ElementState::Pressed;
            let button = match button {
                MouseButton::Left => imgui::MouseButton::Left,
                MouseButton::Right => imgui::MouseButton::Right,
                MouseButton::Middle => imgui::MouseButton::Middle,
                _ => return io.want_capture_mouse,
            };
            io.add_mouse_button_event(button, pressed);
            io.want_capture_mouse
        }
        WindowEvent::MouseWheel { delta, .. } => {
            let (h, v) = match delta {
                MouseScrollDelta::LineDelta(h, v) => (*h, *v),
                MouseScrollDelta::PixelDelta(p) => (p.x as f32 / 16.0, p.y as f32 / 16.0),
            };
            io.add_mouse_wheel_event([h, v]);
            io.want_capture_mouse
        }
        WindowEvent::ModifiersChanged(mods) => {
            let state = mods.state();
            io.add_key_event(Key::ModShift, state.shift_key());
            io.add_key_event(Key::ModCtrl, state.control_key());
            io.add_key_event(Key::ModAlt, state.alt_key());
            io.add_key_event(Key::ModSuper, state.super_key());
            false
        }
        WindowEvent::KeyboardInput { event, .. } => {
            let pressed = event.state == ElementState::Pressed;
            if let PhysicalKey::Code(code) = event.physical_key {
                if let Some(key) = translate_key(code) {
                    io.add_key_event(key, pressed);
                }
            }
            // Printable input is separate from the key events: ImGui needs the
            // resolved character, which depends on the keyboard layout.
            if pressed {
                if let Some(text) = &event.text {
                    for ch in text.chars().filter(|c| !c.is_control()) {
                        io.add_input_character(ch);
                    }
                } else if let winit::keyboard::Key::Named(NamedKey::Space) = event.logical_key {
                    io.add_input_character(' ');
                }
            }
            io.want_capture_keyboard
        }
        WindowEvent::Focused(false) => {
            // Keys held when focus is lost would otherwise stay down forever.
            io.add_key_event(Key::ModShift, false);
            io.add_key_event(Key::ModCtrl, false);
            io.add_key_event(Key::ModAlt, false);
            io.add_key_event(Key::ModSuper, false);
            false
        }
        _ => false,
    }
}

fn translate_key(code: KeyCode) -> Option<Key> {
    use KeyCode as C;
    Some(match code {
        C::Tab => Key::Tab,
        C::ArrowLeft => Key::LeftArrow,
        C::ArrowRight => Key::RightArrow,
        C::ArrowUp => Key::UpArrow,
        C::ArrowDown => Key::DownArrow,
        C::PageUp => Key::PageUp,
        C::PageDown => Key::PageDown,
        C::Home => Key::Home,
        C::End => Key::End,
        C::Insert => Key::Insert,
        C::Delete => Key::Delete,
        C::Backspace => Key::Backspace,
        C::Space => Key::Space,
        C::Enter | C::NumpadEnter => Key::Enter,
        C::Escape => Key::Escape,
        C::ControlLeft => Key::LeftCtrl,
        C::ShiftLeft => Key::LeftShift,
        C::AltLeft => Key::LeftAlt,
        C::ControlRight => Key::RightCtrl,
        C::ShiftRight => Key::RightShift,
        C::AltRight => Key::RightAlt,
        C::Digit0 => Key::Alpha0,
        C::Digit1 => Key::Alpha1,
        C::Digit2 => Key::Alpha2,
        C::Digit3 => Key::Alpha3,
        C::Digit4 => Key::Alpha4,
        C::Digit5 => Key::Alpha5,
        C::Digit6 => Key::Alpha6,
        C::Digit7 => Key::Alpha7,
        C::Digit8 => Key::Alpha8,
        C::Digit9 => Key::Alpha9,
        C::KeyA => Key::A,
        C::KeyB => Key::B,
        C::KeyC => Key::C,
        C::KeyD => Key::D,
        C::KeyE => Key::E,
        C::KeyF => Key::F,
        C::KeyG => Key::G,
        C::KeyH => Key::H,
        C::KeyI => Key::I,
        C::KeyJ => Key::J,
        C::KeyK => Key::K,
        C::KeyL => Key::L,
        C::KeyM => Key::M,
        C::KeyN => Key::N,
        C::KeyO => Key::O,
        C::KeyP => Key::P,
        C::KeyQ => Key::Q,
        C::KeyR => Key::R,
        C::KeyS => Key::S,
        C::KeyT => Key::T,
        C::KeyU => Key::U,
        C::KeyV => Key::V,
        C::KeyW => Key::W,
        C::KeyX => Key::X,
        C::KeyY => Key::Y,
        C::KeyZ => Key::Z,
        C::F1 => Key::F1,
        C::F2 => Key::F2,
        C::F3 => Key::F3,
        C::F4 => Key::F4,
        C::F5 => Key::F5,
        C::F6 => Key::F6,
        C::F7 => Key::F7,
        C::F8 => Key::F8,
        C::F9 => Key::F9,
        C::F10 => Key::F10,
        C::F11 => Key::F11,
        C::F12 => Key::F12,
        _ => return None,
    })
}
