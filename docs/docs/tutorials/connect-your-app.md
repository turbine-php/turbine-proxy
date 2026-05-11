---
sidebar_position: 3
---

# Connect Your App to TurbineProxy

TurbineProxy implements the MySQL and PostgreSQL wire protocols natively. Your application connects to it exactly like it connects to the real database — using the same driver, the same SQL, and the same authentication. **No code changes are required.**

## Changing the Connection String

The only change needed is the **host and port**. Everything else stays the same.

### MySQL (before)
```
mysql://root:password@localhost:3306/myapp
```

### MySQL (after — pointing to TurbineProxy)
```
mysql://root:password@localhost:3307/myapp
```

### PostgreSQL (before)
```
postgresql://postgres:password@localhost:5432/myapp
```

### PostgreSQL (after)
```
postgresql://postgres:password@localhost:5433/myapp
```

## Framework Examples

### Laravel (PHP)

In `.env`:

```env
DB_HOST=127.0.0.1
DB_PORT=3307
DB_DATABASE=myapp
DB_USERNAME=root
DB_PASSWORD=yourpassword
```

### Django (Python)

In `settings.py`:

```python
DATABASES = {
    'default': {
        'ENGINE': 'django.db.backends.mysql',
        'HOST': '127.0.0.1',
        'PORT': '3307',
        'NAME': 'myapp',
        'USER': 'root',
        'PASSWORD': 'yourpassword',
    }
}
```

### Node.js (mysql2)

```js
const pool = mysql.createPool({
  host: '127.0.0.1',
  port: 3307,
  user: 'root',
  password: 'yourpassword',
  database: 'myapp',
});
```

### Go (database/sql + go-sql-driver/mysql)

```go
db, err := sql.Open("mysql", "root:yourpassword@tcp(127.0.0.1:3307)/myapp")
```

### Ruby on Rails

In `config/database.yml`:

```yaml
default: &default
  adapter: mysql2
  host: 127.0.0.1
  port: 3307
  username: root
  password: yourpassword
  database: myapp
```

## Adding Per-User Access Control (Optional)

By default, TurbineProxy passes authentication credentials directly to the backend (transparent auth). To add per-user rules at the proxy level, define users in the config:

```toml
[[users]]
name         = "app"
password     = "apppass"
allow_writes = true    # Permit INSERT / UPDATE / DELETE

[[users]]
name         = "reporting"
password     = "ropass"
allow_writes = false   # SELECT only
max_connections = 10   # Limit this user to 10 simultaneous connections
```

When `[[users]]` are defined, TurbineProxy validates credentials at the proxy boundary. Authentication succeeds if the username and password match a configured user — backend credentials are then used for the actual backend connection.

## Verifying Routing

After connecting your app, open the dashboard at `http://localhost:8080` and watch the **Overview** tab. You should see live query counts appear as your app makes requests.

To check reads vs. writes split:

```bash
curl http://localhost:8080/api/stats | jq '{reads: .queries_read, writes: .queries_write}'
```

## What's Next?

- [Explore the Dashboard](./explore-the-dashboard)
- [Set Up Read/Write Splitting with a Replica](./read-write-splitting)
