use fixture::{Inventory, StockError};

#[test]
fn reservations_hold_stock() {
    let mut inventory = Inventory::new();
    inventory.receive("apple", 5);
    inventory.reserve("apple", 2).unwrap();
    assert_eq!(inventory.available("apple"), 3);
    assert!(matches!(
        inventory.ship("apple", 4),
        Err(StockError::Insufficient { available: 3, .. })
    ));
    inventory.ship_reserved("apple", 2).unwrap();
    assert_eq!(inventory.on_hand("apple"), 3);
    assert_eq!(inventory.reserved("apple"), 0);
    assert!(matches!(
        inventory.release("apple", 1),
        Err(StockError::NotReserved { reserved: 0, .. })
    ));
    assert_eq!(
        inventory.reserve("kiwi", 1),
        Err(StockError::UnknownSku("kiwi".to_string()))
    );
}
