use crate::Inventory;

/// One line per known SKU, in ascending SKU order.
pub fn stock_report(inventory: &Inventory) -> String {
    let mut out = String::new();
    for sku in inventory.skus() {
        out.push_str(&format!(
            "{sku}: {} on hand, {} reserved, {} available\n",
            inventory.on_hand(sku),
            inventory.reserved(sku),
            inventory.available(sku)
        ));
    }
    out
}
