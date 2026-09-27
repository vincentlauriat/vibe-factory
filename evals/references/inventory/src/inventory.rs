use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StockError {
    UnknownSku(String),
    Insufficient {
        sku: String,
        requested: u32,
        available: u32,
    },
    NotReserved {
        sku: String,
        requested: u32,
        reserved: u32,
    },
}

#[derive(Debug, Clone, Copy, Default)]
struct Level {
    on_hand: u32,
    reserved: u32,
}

impl Level {
    fn available(self) -> u32 {
        self.on_hand - self.reserved
    }
}

/// Stock levels per SKU. A SKU becomes known the first time it is received.
#[derive(Debug, Clone, Default)]
pub struct Inventory {
    levels: BTreeMap<String, Level>,
}

impl Inventory {
    pub fn new() -> Self {
        Self::default()
    }

    fn level_mut(&mut self, sku: &str) -> Result<&mut Level, StockError> {
        self.levels
            .get_mut(sku)
            .ok_or_else(|| StockError::UnknownSku(sku.to_string()))
    }

    fn level(&self, sku: &str) -> Level {
        self.levels.get(sku).copied().unwrap_or_default()
    }

    pub fn receive(&mut self, sku: &str, qty: u32) {
        let level = self.levels.entry(sku.to_string()).or_default();
        level.on_hand = level.on_hand.saturating_add(qty);
    }

    fn take_available(&mut self, sku: &str, qty: u32) -> Result<&mut Level, StockError> {
        let level = self.level_mut(sku)?;
        if qty > level.available() {
            return Err(StockError::Insufficient {
                sku: sku.to_string(),
                requested: qty,
                available: level.available(),
            });
        }
        Ok(level)
    }

    fn take_reserved(&mut self, sku: &str, qty: u32) -> Result<&mut Level, StockError> {
        let level = self.level_mut(sku)?;
        if qty > level.reserved {
            return Err(StockError::NotReserved {
                sku: sku.to_string(),
                requested: qty,
                reserved: level.reserved,
            });
        }
        Ok(level)
    }

    pub fn ship(&mut self, sku: &str, qty: u32) -> Result<(), StockError> {
        self.take_available(sku, qty)?.on_hand -= qty;
        Ok(())
    }

    pub fn reserve(&mut self, sku: &str, qty: u32) -> Result<(), StockError> {
        self.take_available(sku, qty)?.reserved += qty;
        Ok(())
    }

    pub fn release(&mut self, sku: &str, qty: u32) -> Result<(), StockError> {
        self.take_reserved(sku, qty)?.reserved -= qty;
        Ok(())
    }

    pub fn ship_reserved(&mut self, sku: &str, qty: u32) -> Result<(), StockError> {
        let level = self.take_reserved(sku, qty)?;
        level.reserved -= qty;
        level.on_hand -= qty;
        Ok(())
    }

    pub fn on_hand(&self, sku: &str) -> u32 {
        self.level(sku).on_hand
    }

    pub fn reserved(&self, sku: &str) -> u32 {
        self.level(sku).reserved
    }

    pub fn available(&self, sku: &str) -> u32 {
        self.level(sku).available()
    }

    /// Known SKUs in ascending order.
    pub fn skus(&self) -> Vec<&str> {
        self.levels.keys().map(String::as_str).collect()
    }
}
