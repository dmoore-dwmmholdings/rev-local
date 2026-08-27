/// Look a user up by name.
pub fn find_user(conn: &Connection, name: &str) -> Result<Vec<Row>, Error> {
    // BUG (planted): `name` is interpolated straight into the SQL.
    let sql = format!("SELECT id, email FROM users WHERE name = '{}'", name);
    conn.query(&sql)
}

pub struct Connection;
pub struct Row;
pub struct Error;

impl Connection {
    pub fn query(&self, _sql: &str) -> Result<Vec<Row>, Error> {
        Ok(Vec::new())
    }
}
