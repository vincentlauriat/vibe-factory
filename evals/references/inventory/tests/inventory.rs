use fixture::report::stock_report;
use fixture::{Inventory, StockError};

#[test]
fn receive_and_ship() {
    let mut inventory = Inventory::new();
    inventory.receive("apple", 5);
    inventory.ship("apple", 3).unwrap();
    assert_eq!(inventory.on_hand("apple"), 2);
    assert_eq!(
        inventory.ship("apple", 3),
        Err(StockError::Insufficient {
            sku: "apple".to_string(),
            requested: 3,
            available: 2,
        })
    );
    assert_eq!(
        inventory.ship("pear", 1),
        Err(StockError::UnknownSku("pear".to_string()))
    );
}

#[test]
fn report_lists_skus_in_order() {
    let mut inventory = Inventory::new();
    inventory.receive("pear", 1);
    inventory.receive("apple", 4);
    assert_eq!(
        stock_report(&inventory),
        "apple: 4 on hand, 0 reserved, 4 available\npear: 1 on hand, 0 reserved, 1 available\n"
    );
}
