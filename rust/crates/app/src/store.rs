//! Optional support through Microsoft Store add-ons (`Windows.Services.Store`). Nothing is locked
//! behind them: every feature stays free, and supporters only get a thank-you badge.
//!
//! Prices are set per add-on in Partner Center (Microsoft converts them per market), so the panel
//! shows the Store's localised price and only falls back to the US-dollar label when the Store
//! can't be reached. Every Store call runs on a short-lived worker thread and reports back over a
//! channel, so the overlay never stalls while the purchase dialog is open.
//!
//! Only a packaged (MSIX) install has a Store identity. Running the plain exe, the panel says so and
//! offers the BackaBuddy link instead.

use deafoverlay::ui::{Support, STORE_SUPPORT};
use std::sync::mpsc::{self, Receiver, Sender};
use windows::core::{Interface, HSTRING};
use windows_collections::IIterable;
use windows::Services::Store::{StoreContext, StorePurchaseStatus};
use windows::Win32::Foundation::{APPMODEL_ERROR_NO_PACKAGE, HWND};
use windows::Win32::Storage::Packaging::Appx::GetCurrentPackageFullName;
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
use windows::Win32::UI::Shell::IInitializeWithWindow;

pub const BACKABUDDY: &str = "https://backabuddy.co.za/campaign/heroes-without-sound";

/// Store IDs of the support add-ons from Partner Center (12 characters, e.g. "9NBLGGH4R2R6"), in
/// `ui::SUPPORT_FALLBACK` order: $3 one-off (Durable), $10 one-off (Durable), $2/month
/// (Subscription). Empty until the add-ons exist; the panel then says the option isn't there yet.
pub const STORE_IDS: [&str; 3] = ["", "", ""];

enum Msg {
    Checked { prices: [Option<String>; 3], supporter: bool },
    Unavailable(&'static str),
    Bought,
    Cancelled,
    Failed,
}

pub struct Store {
    owner: isize, // HWND of the panel: owner of the Store's purchase dialog (kept above our topmost window)
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    pub state: Support,
}

impl Store {
    /// Starts the first check (licence + localised prices) in the background.
    pub fn start(owner: HWND) -> Self {
        let (tx, rx) = mpsc::channel();
        let mut s = Self { owner: owner.0 as isize, tx, rx, state: Support::default() };
        if !STORE_SUPPORT {
            s.state.available = Some(false); // switched off: never contact the Store
        } else if !packaged() {
            s.state.available = Some(false);
            s.state.message = Some("In-app support needs the Microsoft Store version.");
        } else {
            s.check();
        }
        s
    }

    fn check(&self) {
        let (tx, owner) = (self.tx.clone(), self.owner);
        std::thread::spawn(move || {
            let _ = tx.send(match query(owner) {
                Ok((prices, supporter)) => Msg::Checked { prices, supporter },
                Err(_) => Msg::Unavailable("Couldn't reach the Microsoft Store."),
            });
        });
    }

    pub fn buy(&mut self, i: usize) {
        if self.state.busy || self.state.available != Some(true) {
            return;
        }
        let id = STORE_IDS[i];
        if id.is_empty() {
            self.state.message = Some("This option isn't in the Store yet.");
            return;
        }
        self.state.busy = true;
        self.state.message = None;
        let (tx, owner) = (self.tx.clone(), self.owner);
        std::thread::spawn(move || {
            let _ = tx.send(purchase(owner, id).unwrap_or(Msg::Failed));
        });
    }

    /// Fold in whatever the workers reported since last frame.
    pub fn poll(&mut self) {
        while let Ok(m) = self.rx.try_recv() {
            match m {
                Msg::Checked { prices, supporter } => {
                    self.state.available = Some(true);
                    self.state.prices = prices;
                    self.state.supporter = supporter;
                }
                Msg::Unavailable(why) => {
                    self.state.available = Some(false);
                    self.state.message = Some(why);
                }
                Msg::Bought => {
                    self.state.busy = false;
                    self.state.supporter = true;
                    self.state.message = Some("Thank you for supporting Heroes Without Sound!");
                    self.check(); // refresh licences from the Store
                }
                Msg::Cancelled => {
                    self.state.busy = false;
                    self.state.message = Some("No purchase was made.");
                }
                Msg::Failed => {
                    self.state.busy = false;
                    self.state.message = Some("The Store couldn't complete that. Please try again.");
                }
            }
        }
    }
}

/// True when running from an MSIX package (and so with a Store identity).
fn packaged() -> bool {
    let mut len = 0u32;
    unsafe { GetCurrentPackageFullName(&mut len, None) != APPMODEL_ERROR_NO_PACKAGE }
}

/// A Store context whose dialogs belong to our panel window (desktop apps must say which window).
fn context(owner: isize) -> windows::core::Result<StoreContext> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    let ctx = StoreContext::GetDefault()?;
    let init: IInitializeWithWindow = ctx.cast()?;
    unsafe { init.Initialize(HWND(owner as _))? };
    Ok(ctx)
}

fn query(owner: isize) -> windows::core::Result<([Option<String>; 3], bool)> {
    let ctx = context(owner)?;
    let licence = ctx.GetAppLicenseAsync()?.join()?;
    let mut supporter = false;
    for kv in licence.AddOnLicenses()? {
        supporter |= kv.Value()?.IsActive()?;
    }
    let kinds = IIterable::<HSTRING>::from(vec![HSTRING::from("Durable"), HSTRING::from("Subscription")]);
    let found = ctx.GetAssociatedStoreProductsAsync(&kinds)?.join()?;
    let mut prices: [Option<String>; 3] = Default::default();
    for kv in found.Products()? {
        let product = kv.Value()?;
        let id = product.StoreId()?.to_string();
        if let Some(i) = STORE_IDS.iter().position(|s| !s.is_empty() && *s == id) {
            prices[i] = Some(product.Price()?.FormattedPrice()?.to_string());
        }
    }
    Ok((prices, supporter))
}

fn purchase(owner: isize, store_id: &str) -> windows::core::Result<Msg> {
    let ctx = context(owner)?;
    let result = ctx.RequestPurchaseAsync(&HSTRING::from(store_id))?.join()?;
    Ok(match result.Status()? {
        StorePurchaseStatus::Succeeded | StorePurchaseStatus::AlreadyPurchased => Msg::Bought,
        StorePurchaseStatus::NotPurchased => Msg::Cancelled,
        _ => Msg::Failed,
    })
}
