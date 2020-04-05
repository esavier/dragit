extern crate cairo;
extern crate gdk;
extern crate gio;
extern crate gtk;

use futures::channel::mpsc::{channel, Sender};
use percent_encoding::percent_decode_str;

use std::env::args;
use std::error::Error;
use std::sync::{Arc, Mutex};
use std::thread;

use self::gio::prelude::*;
use self::gtk::prelude::*;

use self::gdk::ScreenExt;
use self::gtk::ApplicationWindow;
use self::gtk::GtkWindowExt;

use crate::p2p::{run_server, FileToSend};
use crate::transfer::Protocol;

fn transfer_file(protocol: impl Protocol, path: &str) -> Result<(), Box<dyn Error>> {
    protocol.transfer_file(path)
}

// TODO: reintegrate Bluetooth
// fn spawn_send_job(file_path: &str) -> thread::Result<()> {
//     let trimmed_path = file_path.replace("file://", "").trim().to_string();
//     let path_arc = Arc::new(trimmed_path);
//     let path_clone = Arc::clone(&path_arc);

//     thread::spawn(move || {
//         println!("Spawning thread");
//         match transfer_file(BluetoothProtocol, &path_clone) {
//             Ok(_) => (),
//             Err(err) => eprintln!("{}", err),
//         }
//     })
//     .join()
// }

// fn push_p2p_job(file_path: String, sender: Arc<Mutex<Sender<FileToSend>>>) -> Result<(), Box<dyn Error>> {
//     let file = FileToSend::new(&file_path)?;
//     let mut sender = sender.lock().unwrap();
//     sender.send(file);

//     Ok(())
// }

pub fn build_window(
    application: &gtk::Application,
    sender: Arc<Mutex<Sender<FileToSend>>>,
) -> Result<(), Box<dyn Error>> {
    let window = gtk::ApplicationWindow::new(application);
    let targets = vec![
        gtk::TargetEntry::new("STRING", gtk::TargetFlags::OTHER_APP, 0),
        gtk::TargetEntry::new("text/uri-list", gtk::TargetFlags::OTHER_APP, 0),
    ];
    let label = gtk::Label::new("[]");
    label.drag_dest_set(gtk::DestDefaults::ALL, &targets, gdk::DragAction::COPY);

    label.connect_drag_motion(|w, _, _, _, _| {
        w.set_text("[FILE]>");
        gtk::Inhibit(false)
    });

    let weak_window = window.downgrade();

    label.connect_drag_data_received(move |w, _, _, _, s, _, _| {
        let path: String = match s.get_text() {
            Some(value) => {
                let value = percent_decode_str(&value)
                    .decode_utf8()
                    .expect("Decoding path failed");
                let path = value.replace("file://", "");
                path.trim().to_string()
            }
            None => s.get_uris().pop().unwrap(),
        };

        let file = match FileToSend::new(&path) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("Failed creating FileToSend {:?}", e);
                return ();
            }
        };
        let mut sender = sender.lock().unwrap();
        sender.try_send(file).expect("Sending failed");

        w.set_text("[]");
        if let Some(win) = weak_window.upgrade() {
            win.resize(5, 1000);
        }
    });

    // Stack the button and label horizontally
    let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    hbox.pack_start(&label, true, true, 0);

    set_visual(&window, &None);
    window.connect_screen_changed(set_visual);
    window.connect_draw(draw);

    window.set_title("Dragit!");
    window.set_default_size(5, 1000);
    window.add(&hbox);
    window.set_app_paintable(true);
    window.set_decorated(false);
    window.set_skip_taskbar_hint(true);
    window.move_(0, 0);
    window.set_keep_above(true);
    window.show_all();

    // GTK & main window boilerplate
    window.connect_delete_event(move |win, _| {
        win.destroy();
        Inhibit(false)
    });
    Ok(())
}

fn set_visual(window: &ApplicationWindow, _screen: &Option<gdk::Screen>) {
    if let Some(screen) = window.get_screen() {
        if let Some(visual) = screen.get_rgba_visual() {
            window.set_visual(&visual);
        }
    }
}

fn draw(_window: &ApplicationWindow, ctx: &cairo::Context) -> Inhibit {
    ctx.set_source_rgba(0.0, 0.0, 0.0, 0.4);
    ctx.set_operator(cairo::enums::Operator::Screen);
    ctx.paint();
    Inhibit(false)
}

pub fn start_window() {
    let (sender, receiver) = channel::<FileToSend>(1024);

    // Start the p2p server in separate thread
    thread::spawn(move || match run_server(receiver) {
        Ok(_) => {}
        Err(e) => eprintln!("{:?}", e),
    });

    let application = gtk::Application::new("com.drag_and_drop", gio::ApplicationFlags::empty())
        .expect("Initialization failed...");
    application.connect_startup(move |app| {
        let sender_c = Arc::new(Mutex::new(sender.clone()));
        match build_window(app, sender_c) {
            Ok(_) => println!("Ok!"),
            Err(e) => println!("{:?}", e),
        };
    });
    application.connect_activate(|_| {});

    application.run(&args().collect::<Vec<_>>());
}
