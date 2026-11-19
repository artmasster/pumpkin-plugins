//! The market's rules and records: listings, the boxes items go back to, fees and taxes. No
//! server needed here.
//!
//! A player lists what they hold for a price; others buy it with money of the Basics plugin. A
//! listing that is not sold in time goes to its seller's box. Items are kept as the text of
//! `mcsv_common::items` (every part of the item), so they come back exactly as they went in.
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub const NS: &str = "market";
/// Longest item text the market keeps; longer items (a full shulker box, say) cannot be sold.
pub const MAX_ITEM: usize = 30_000;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Config {
    pub language: String,
    /// Listings one player may have at once.
    pub max_listings: u32,
    /// Hours a listing stays up.
    pub listing_hours: f64,
    /// Part of the price paid when listing (0.02 = 2 %), not given back.
    pub listing_fee: f64,
    /// Part of the price kept from the seller when it sells.
    pub sales_tax: f64,
    pub min_price: f64,
    pub max_price: f64,
    pub currency_symbol: String,
}

impl Default for Config {
    fn default() -> Self {
        Config { language: "th".into(), max_listings: 10, listing_hours: 48.0, listing_fee: 0.0, sales_tax: 0.05, min_price: 1.0, max_price: 1e9, currency_symbol: "$".into() }
    }
}

impl Config {
    pub fn cleaned(mut self) -> Config {
        let d = Config::default();
        self.language = if self.language.eq_ignore_ascii_case("en") { "en".into() } else { "th".into() };
        self.max_listings = self.max_listings.clamp(1, 200);
        let fix = |v: f64, lo: f64, hi: f64, def: f64| if v.is_finite() { v.clamp(lo, hi) } else { def };
        self.listing_hours = fix(self.listing_hours, 0.1, 24.0 * 60.0, d.listing_hours);
        self.listing_fee = fix(self.listing_fee, 0.0, 0.5, d.listing_fee);
        self.sales_tax = fix(self.sales_tax, 0.0, 0.5, d.sales_tax);
        self.min_price = fix(self.min_price, 0.01, 1e12, d.min_price);
        self.max_price = fix(self.max_price, self.min_price, 1e13, d.max_price);
        self.currency_symbol = self.currency_symbol.chars().take(4).collect();
        self
    }

    pub fn thai(&self) -> bool {
        self.language == "th"
    }

    pub fn money(&self, n: f64) -> String {
        let whole = n.trunc() as i64;
        let cents = ((n - n.trunc()) * 100.0).round() as i64;
        let mut digits = whole.abs().to_string();
        let mut out = String::new();
        while digits.len() > 3 {
            let tail = digits.split_off(digits.len() - 3);
            out = format!(",{tail}{out}");
        }
        out = format!("{}{digits}{out}", if whole < 0 { "-" } else { "" });
        if cents != 0 { format!("{}{out}.{:02}", self.currency_symbol, cents.abs()) } else { format!("{}{out}", self.currency_symbol) }
    }

    /// What listing costs, and what the seller gets when it sells.
    pub fn fee(&self, price: f64) -> f64 {
        round2(price * self.listing_fee)
    }

    pub fn payout(&self, price: f64) -> f64 {
        round2(price * (1.0 - self.sales_tax))
    }
}

pub fn round2(n: f64) -> f64 {
    (n * 100.0).round() / 100.0
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Listing {
    pub id: u64,
    pub seller: String,
    pub seller_name: String,
    pub item: String,
    pub count: u8,
    pub price: f64,
    /// Epoch seconds.
    pub listed: u64,
    pub ends: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Boxed {
    pub item: String,
    pub count: u8,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Book {
    pub next_id: u64,
    pub listings: Vec<Listing>,
    /// Items waiting for their owners (unsold, taken back).
    pub boxes: BTreeMap<String, Vec<Boxed>>,
    /// Money Basics did not take yet, by player key and name.
    pub owed: BTreeMap<String, (String, f64)>,
    /// Sold in all.
    pub sales: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Refusal {
    Price,
    TooMany,
    NoItem,
    TooBig,
}

/// Can this be listed? `price` is for the whole stack.
pub fn may_list(cfg: &Config, book: &Book, seller: &str, item: Option<&str>, price: f64) -> Result<(), Refusal> {
    if !price.is_finite() || price < cfg.min_price || price > cfg.max_price {
        return Err(Refusal::Price);
    }
    let Some(item) = item else { return Err(Refusal::NoItem) };
    if item.ends_with("|!") || item.len() > MAX_ITEM {
        return Err(Refusal::TooBig);
    }
    if book.listings.iter().filter(|l| l.seller == seller).count() >= cfg.max_listings as usize {
        return Err(Refusal::TooMany);
    }
    Ok(())
}

impl Book {
    pub fn add(&mut self, cfg: &Config, seller: &str, seller_name: &str, item: String, count: u8, price: f64, now: u64) -> u64 {
        self.next_id += 1;
        let id = self.next_id;
        self.listings.push(Listing { id, seller: seller.to_owned(), seller_name: seller_name.to_owned(), item, count, price: round2(price), listed: now, ends: now + (cfg.listing_hours * 3600.0) as u64 });
        id
    }

    /// Listings whose time is up go to their sellers' boxes. Returns how many.
    pub fn expire(&mut self, now: u64) -> usize {
        let (gone, keep): (Vec<Listing>, Vec<Listing>) = std::mem::take(&mut self.listings).into_iter().partition(|l| l.ends <= now);
        self.listings = keep;
        let n = gone.len();
        for l in gone {
            self.boxes.entry(l.seller).or_default().push(Boxed { item: l.item, count: l.count });
        }
        n
    }

    pub fn take(&mut self, id: u64) -> Option<Listing> {
        let at = self.listings.iter().position(|l| l.id == id)?;
        Some(self.listings.remove(at))
    }

    /// Newest first.
    pub fn page(&self, page: usize, per: usize) -> Vec<&Listing> {
        let mut all: Vec<&Listing> = self.listings.iter().collect();
        all.sort_by(|a, b| b.listed.cmp(&a.listed).then(b.id.cmp(&a.id)));
        all.into_iter().skip(page * per).take(per).collect()
    }

    pub fn owe(&mut self, key: &str, name: &str, amount: f64) {
        let entry = self.owed.entry(key.to_owned()).or_insert_with(|| (name.to_owned(), 0.0));
        entry.1 = round2(entry.1 + amount);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listing_rules() {
        let cfg = Config { max_listings: 2, ..Config::default() };
        let mut book = Book::default();
        assert_eq!(may_list(&cfg, &book, "a", Some("diamond"), 0.5), Err(Refusal::Price));
        assert_eq!(may_list(&cfg, &book, "a", Some("diamond"), f64::NAN), Err(Refusal::Price));
        assert_eq!(may_list(&cfg, &book, "a", None, 10.0), Err(Refusal::NoItem));
        assert_eq!(may_list(&cfg, &book, "a", Some("shulker_box|!"), 10.0), Err(Refusal::TooBig));
        book.add(&cfg, "a", "A", "diamond".into(), 1, 10.0, 0);
        book.add(&cfg, "a", "A", "diamond".into(), 1, 10.0, 0);
        assert_eq!(may_list(&cfg, &book, "a", Some("diamond"), 10.0), Err(Refusal::TooMany));
        assert_eq!(may_list(&cfg, &book, "b", Some("diamond"), 10.0), Ok(()));
    }

    #[test]
    fn unsold_items_go_back_to_their_sellers() {
        let cfg = Config { listing_hours: 1.0, ..Config::default() };
        let mut book = Book::default();
        book.add(&cfg, "a", "A", "diamond".into(), 3, 10.0, 100);
        let id = book.add(&cfg, "b", "B", "emerald".into(), 1, 5.0, 200);
        assert_eq!(book.expire(100 + 3599), 0);
        assert_eq!(book.expire(100 + 3600), 1);
        assert_eq!(book.boxes["a"], vec![Boxed { item: "diamond".into(), count: 3 }]);
        assert_eq!(book.take(id).map(|l| l.price), Some(5.0));
        assert!(book.take(id).is_none());
    }

    #[test]
    fn money_and_cuts() {
        let cfg = Config { listing_fee: 0.02, sales_tax: 0.05, ..Config::default() };
        assert_eq!(cfg.fee(1000.0), 20.0);
        assert_eq!(cfg.payout(1000.0), 950.0);
        assert_eq!(cfg.money(1234567.5), "$1,234,567.50");
        assert_eq!(cfg.money(12.0), "$12");
        let mut book = Book::default();
        book.owe("k", "Al", 1.25);
        book.owe("k", "Al", 2.0);
        assert_eq!(book.owed["k"], ("Al".to_owned(), 3.25));
    }

    #[test]
    fn pages_are_newest_first() {
        let cfg = Config::default();
        let mut book = Book::default();
        for t in 0..5 {
            book.add(&cfg, "a", "A", format!("item{t}"), 1, 10.0, t);
        }
        let p0: Vec<&str> = book.page(0, 2).iter().map(|l| l.item.as_str()).collect();
        assert_eq!(p0, vec!["item4", "item3"]);
        assert_eq!(book.page(2, 2).len(), 1);
    }
}
