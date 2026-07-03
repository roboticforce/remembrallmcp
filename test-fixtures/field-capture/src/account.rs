pub struct Account {
    pub balance: i64,
}

impl Account {
    pub fn double(&self) -> i64 {
        self.balance * 2
    }
}