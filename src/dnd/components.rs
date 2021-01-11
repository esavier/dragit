use std::error::Error;
use std::sync::{Arc, Mutex};

use async_std::sync::Sender;
use bytesize::ByteSize;

use gdk::DragAction;
use glib::object::IsA;
use gtk::prelude::*;
use gtk::{DestDefaults, Label, TargetEntry, TargetFlags};

use libp2p::{multiaddr::Protocol, Multiaddr};
use percent_encoding::percent_decode_str;

use crate::p2p::{FileToSend, OperatingSystem, Peer};
use crate::user_data::UserConfig;

pub const STYLE: &str = "
#downloads border {
    margin-left: 10px;
    margin-right: 10px;
    padding: 20px;
    border-style: dashed;
    border-radius: 15px;
}
#item-frame border {
    border-style: none;
}
#notification {
    padding: 10px;
    border-radius: 10px;
    color: rgb(0, 0, 0);
    background-color: rgba(100, 100, 100, 1.0);
}
#button-close {
    padding: 0;
    margin: 0;
    border: none;
    border-radius: 10px;
}
#button-close:hover {
    background-image: none;
}
progressbar {
    color: rgb(0, 0, 0);
}
#items-list {
    padding: 10px;
    margin: 10px;
    border: 0.5px;
    border-radius: 15px;
    border-style: solid;
    border-color: rgb(180, 180, 180);
}
";

pub struct MainLayout {
    pub layout: gtk::Box,
    pub item_layout: gtk::ListBox,
}

impl MainLayout {
    pub fn new() -> Result<MainLayout, Box<dyn Error>> {
        let layout = gtk::Box::new(gtk::Orientation::Vertical, 10);

        let item_layout = gtk::ListBox::new();
        item_layout.set_selection_mode(gtk::SelectionMode::None);
        item_layout.set_widget_name("items-list");

        // Add separator only when there is more than one item
        item_layout.set_header_func(Some(Box::new(|current_row, next_row| {
            if let Some(row) = next_row {
                let row_name = get_item_name(row);
                if row_name != "empty-item" {
                    let separator = gtk::Separator::new(gtk::Orientation::Vertical);
                    current_row.set_header(Some(&separator));
                }
            }
        })));

        let header_layout = gtk::Box::new(gtk::Orientation::Vertical, 0);

        layout.set_halign(gtk::Align::Center);
        header_layout.set_margin_top(10);

        let frame = gtk::Frame::new(Some("Downloads directory"));
        frame.set_widget_name("downloads");

        let file_chooser =
            gtk::FileChooserButton::new("Choose file", gtk::FileChooserAction::SelectFolder);

        let config = UserConfig::new()?;
        let downloads = config.get_downloads_dir();
        file_chooser.set_filename(downloads);

        file_chooser.connect_file_set(move |chooser| {
            match chooser.get_filename() {
                Some(path) => {
                    info!("Setting downloads directory: {:?}", path);
                    if let Err(e) = config.set_downloads_dir(path.as_path()) {
                        error!("Failed to set downloads directory: {:?}", e);
                    };
                }
                None => {
                    error!("Failed to get new downloads dir");
                }
            };
        });
        frame.add(&file_chooser);
        header_layout.pack_start(&frame, true, true, 10);

        let scroll = gtk::ScrolledWindow::new(gtk::NONE_ADJUSTMENT, gtk::NONE_ADJUSTMENT);
        scroll.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
        scroll.set_min_content_width(550);

        let item_frame = gtk::Frame::new(Some("Devices"));
        item_frame.set_widget_name("item-frame");
        item_frame.add(&item_layout);
        let scroll_box = gtk::Box::new(gtk::Orientation::Vertical, 0);

        scroll_box.pack_start(&header_layout, false, false, 0);
        scroll_box.pack_start(&item_frame, false, false, 0);
        scroll.add(&scroll_box);

        layout.pack_start(&scroll, true, true, 10);

        Ok(MainLayout {
            layout,
            item_layout,
        })
    }
}

#[derive(Debug)]
pub struct PeerItem {
    pub container: gtk::ListBoxRow,
    pub label: Label,
}

impl PeerItem {
    // TODO: is this safe to use &str here?
    pub fn new(name: &str, address: &Multiaddr, hostname: &str, os: &OperatingSystem) -> PeerItem {
        let ip = PeerItem::extract_ip(&address);
        let display_name = format!(
            concat!(
                "<big><b>Device Name</b>: {}</big>\n",
                "<big><b>IP Address</b>: {}</big>\n",
                "<big><b>System</b>: {:?}</big>\n",
            ),
            hostname, ip, os
        );

        let label = Label::new(None);
        label.set_markup(&display_name);
        label.set_widget_name("drop-label");
        label.set_halign(gtk::Align::Center);
        label.set_size_request(500, 100);

        let image = gtk::Image::new_from_icon_name(Some("insert-object"), gtk::IconSize::Dialog);

        let container = gtk::ListBoxRow::new();
        container.set_vexpand(true);

        let inner_container = gtk::Box::new(gtk::Orientation::Vertical, 10);

        container.set_widget_name(name);
        inner_container.set_widget_name("drop-zone");

        inner_container.pack_start(&image, true, true, 10);
        inner_container.pack_start(&label, true, true, 10);
        container.add(&inner_container);

        PeerItem { container, label }
    }

    fn extract_ip(address: &Multiaddr) -> String {
        let components = address.iter().collect::<Vec<Protocol>>();
        let ip = &components[0];
        ip.to_string().replace("/ip4/", "").replace("/ip6/", "")
    }

    pub fn bind_drag_and_drop(
        self,
        peer: &Peer,
        file_sender: Arc<Mutex<Sender<FileToSend>>>,
    ) -> Self {
        let peer_id = peer.peer_id.clone();
        let targets = vec![
            TargetEntry::new("STRING", TargetFlags::OTHER_APP, 0),
            TargetEntry::new("text/html", TargetFlags::OTHER_APP, 0),
            TargetEntry::new("image/png", TargetFlags::OTHER_APP, 0),
            // TODO: use different content type here
            TargetEntry::new("text/uri-list", TargetFlags::OTHER_APP, 0),
        ];
        self.container
            .drag_dest_set(DestDefaults::ALL, &targets, DragAction::COPY);

        self.container
            .connect_drag_data_received(move |_win, _, _, _, s, _, _| {
                let path: String = match s.get_text() {
                    Some(value) => PeerItem::clean_filename(&value).expect("Decoding path failed"),
                    None => {
                        // Extracting the file path from the URI works best for Windows
                        let uri = s.get_uris().pop().unwrap().to_string();
                        PeerItem::clean_filename(&uri).expect("Decoding path from URI failed")
                    }
                };
                let file = match FileToSend::new(&path, &peer_id) {
                    Ok(v) => v,
                    Err(e) => {
                        error!("Failed creating FileToSend {:?}", e);
                        return ();
                    }
                };
                let sender = file_sender.lock().unwrap();
                sender.try_send(file).expect("Sending failed");
            });

        self
    }

    fn clean_filename(path: &str) -> Result<String, Box<dyn Error>> {
        let value = percent_decode_str(path).decode_utf8()?;

        let path = clean_file_proto(&value);
        Ok(path.trim().to_string())
    }
}

#[cfg(not(target_os = "windows"))]
fn clean_file_proto(value: &str) -> String {
    value.replace("file://", "")
}

#[cfg(target_os = "windows")]
fn clean_file_proto(value: &str) -> String {
    // Windows paths contain one extra slash
    value.replace("file:///", "")
}

pub struct ProgressNotification {
    revealer: gtk::Revealer,
    overlay: gtk::Overlay,
    pub progress_bar: gtk::ProgressBar,
}

impl ProgressNotification {
    pub fn new(main_overlay: &gtk::Overlay) -> Self {
        let layout = gtk::Box::new(gtk::Orientation::Horizontal, 5);
        layout.set_widget_name("notification");

        let overlay = gtk::Overlay::new();
        let revealer = gtk::Revealer::new();
        let progress_bar = gtk::ProgressBar::new();

        revealer.set_halign(gtk::Align::Center);
        revealer.set_valign(gtk::Align::Start);
        revealer.set_transition_type(gtk::RevealerTransitionType::SlideDown);

        progress_bar.set_text(Some("Receiving file"));
        progress_bar.set_show_text(true);

        progress_bar.set_halign(gtk::Align::Center);
        progress_bar.set_valign(gtk::Align::Start);
        progress_bar.set_hexpand(true);
        progress_bar.set_size_request(500, 50);
        revealer.set_margin_bottom(30);

        layout.pack_start(&progress_bar, true, false, 0);
        revealer.add(&layout);

        overlay.add_overlay(&revealer);

        main_overlay.add_overlay(&overlay);
        revealer.set_reveal_child(false);

        ProgressNotification {
            revealer,
            overlay,
            progress_bar,
        }
    }

    fn show(&self, main_overlay: &gtk::Overlay) {
        main_overlay.reorder_overlay(&self.overlay, 10);
        self.revealer.set_reveal_child(true)
    }

    fn show_progress(&self, main_overlay: &gtk::Overlay, size: f64, total: f64, text: &str) {
        self.show(main_overlay);
        self.progress_bar.set_fraction(size / total);
        self.progress_bar.set_text(Some(text));
    }

    pub fn show_incoming(&self, main_overlay: &gtk::Overlay, size: f64, total: f64) {
        self.show_progress(main_overlay, size, total, "Receiving file");
    }

    pub fn show_outgoing(&self, main_overlay: &gtk::Overlay, size: f64, total: f64) {
        self.show_progress(main_overlay, size, total, "Sending file");
    }

    pub fn hide(&self, main_overlay: &gtk::Overlay) {
        main_overlay.reorder_overlay(&self.overlay, 0);

        self.revealer.set_reveal_child(false)
    }
}

pub enum NotificationType {
    Alert,
    Error,
}

pub struct AppNotification {
    revealer: gtk::Revealer,
    pub overlay: gtk::Overlay,
    label: Label,
}

impl AppNotification {
    pub fn new(main_overlay: &gtk::Overlay, notification_type: NotificationType) -> Self {
        let layout = gtk::Box::new(gtk::Orientation::Horizontal, 5);
        let overlay = gtk::Overlay::new();
        let revealer = gtk::Revealer::new();
        let label = Label::new(Some("File correct"));

        layout.set_widget_name("notification");

        let button_close = gtk::Button::new_from_icon_name(
            Some("window-close-symbolic"),
            gtk::IconSize::SmallToolbar,
        );
        button_close.set_widget_name("button-close");
        button_close.set_relief(gtk::ReliefStyle::None);

        let revealer_weak = revealer.downgrade();
        let main_overlay_weak = main_overlay.downgrade();
        let overlay_weak = overlay.downgrade();

        button_close.connect_clicked(move |_| {
            if let (Some(r), Some(mo), Some(o)) = (
                revealer_weak.upgrade(),
                main_overlay_weak.upgrade(),
                overlay_weak.upgrade(),
            ) {
                r.set_reveal_child(false);
                mo.reorder_overlay(&o, 0);
            }
        });

        revealer.set_halign(gtk::Align::Center);
        revealer.set_valign(gtk::Align::Start);

        label.set_halign(gtk::Align::Start);
        label.set_valign(gtk::Align::Center);
        label.set_size_request(400, 50);

        revealer.set_transition_type(gtk::RevealerTransitionType::SlideDown);

        let icon = AppNotification::set_icon(notification_type);

        layout.pack_start(&icon, true, false, 0);
        layout.pack_start(&label, true, false, 0);
        layout.pack_start(&button_close, true, false, 0);

        revealer.add(&layout);
        overlay.add_overlay(&revealer);

        main_overlay.add_overlay(&overlay);
        revealer.set_reveal_child(false);

        AppNotification {
            revealer,
            overlay,
            label,
        }
    }

    fn set_icon(notification_type: NotificationType) -> gtk::Image {
        match notification_type {
            NotificationType::Alert => {
                gtk::Image::new_from_icon_name(Some("dialog-information"), gtk::IconSize::Button)
            }
            NotificationType::Error => {
                gtk::Image::new_from_icon_name(Some("dialog-warning"), gtk::IconSize::Button)
            }
        }
    }

    fn reveal(&self, overlay: &gtk::Overlay) {
        overlay.reorder_overlay(&self.overlay, 10);
        self.revealer.set_reveal_child(true);
    }

    pub fn show(&self, overlay: &gtk::Overlay, text: String) {
        self.label.set_text(&text);

        self.reveal(overlay);
    }
}

pub struct AcceptFileDialog(gtk::MessageDialog);

impl AcceptFileDialog {
    pub fn new(window: &gtk::ApplicationWindow, name: String, size: usize) -> AcceptFileDialog {
        let readable_size = ByteSize(size as u64);
        let dialog = gtk::MessageDialog::new(
            Some(window),
            gtk::DialogFlags::MODAL,
            gtk::MessageType::Question,
            gtk::ButtonsType::YesNo,
            &format!(
                "Incoming file {} ({}).\n\nWould you like to accept the file?",
                name, readable_size
            ),
        );
        AcceptFileDialog(dialog)
    }

    pub fn run(&self) -> gtk::ResponseType {
        let resp = self.0.run();
        self.0.destroy();
        resp
    }
}

/// Element shown when there are no devices to display yet
/// TODO: Probably can be replaced with gtk placeholder
pub struct EmptyListItem {
    pub revealer: gtk::Revealer,
    pub container: gtk::ListBoxRow,
}

impl EmptyListItem {
    pub fn new() -> EmptyListItem {
        let label = Label::new(Some("Looking for devices..."));
        let revealer = gtk::Revealer::new();
        let container = gtk::ListBoxRow::new();
        let inner_container = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let spinner = gtk::Spinner::new();
        label.set_halign(gtk::Align::Center);
        label.set_size_request(500, 100);

        let image =
            gtk::Image::new_from_icon_name(Some("network-transmit-receive"), gtk::IconSize::Dialog);

        revealer.set_halign(gtk::Align::Center);
        revealer.set_valign(gtk::Align::Start);

        spinner.start();

        let text = concat!(
            "Please run <b>Dragit</b> on another device\n",
            "and wait until applications discover each other.\n\n",
            "Once device appears here, drop a file on it.\n",
        );
        let description = gtk::Label::new(None);
        description.set_markup(&text);

        inner_container.pack_start(&description, false, false, 10);
        inner_container.pack_start(&image, false, false, 0);
        inner_container.pack_start(&label, false, false, 0);
        inner_container.pack_start(&spinner, false, false, 0);

        revealer.add(&inner_container);

        revealer.set_transition_type(gtk::RevealerTransitionType::SlideDown);
        container.set_widget_name("empty-item");
        revealer.set_reveal_child(true);

        container.add(&revealer);

        EmptyListItem {
            revealer,
            container,
        }
    }

    pub fn show(&self) {
        self.revealer.set_reveal_child(true);
    }

    pub fn hide(&self) {
        self.revealer.set_reveal_child(false);
    }
}

pub fn get_item_name<I: IsA<gtk::Widget>>(item: &I) -> String {
    item.get_widget_name()
        .unwrap_or(glib::GString::from(""))
        .to_string()
}
