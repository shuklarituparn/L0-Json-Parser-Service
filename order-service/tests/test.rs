#[cfg(test)]
mod tests {
    use reqwest::{Client, StatusCode};
    use serde_json::json;
    use serial_test::serial;
    use std::env;

    fn get_port() -> u16 {
        env::var("TEST_PORT")
            .unwrap_or_else(|_| "3000".to_string()) // Default port is 3000 if not set
            .parse()
            .expect("Failed to parse port")
    }

    #[tokio::test]
    #[serial]
    async fn health_check() {
        let client = Client::new();
        let port = get_port();
        let url = format!("http://localhost:{}/health", port);

        let response = client
            .get(&url)
            .send()
            .await
            .expect("Failed to send request");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.text().await.unwrap(), "OK");
    }

    #[tokio::test]
    #[serial]
    async fn create_order_creation() {
        let client = Client::new();
        let port = get_port();
        let url = format!("http://localhost:{}/order", port);

        let json_payload = json!({
          "order_uid": "b563feb7b2b84b6test",
          "track_number": "WBILMTESTTRACK",
          "entry": "WBIL",
          "delivery": {
            "name": "Test Testov",
            "phone": "+9720000000",
            "zip": "2639809",
            "city": "Kiryat Mozkin",
            "address": "Ploshad Mira 15",
            "region": "Kraiot",
            "email": "test@gmail.com"
          },
          "payment": {
            "transaction": "b563feb7b2b84b6test",
            "request_id": "",
            "currency": "USD",
            "provider": "wbpay",
            "amount": 1817,
            "payment_dt": 1637907727,
            "bank": "alpha",
            "delivery_cost": 1500,
            "goods_total": 317,
            "custom_fee": 0
          },
          "items": [
            {
              "chrt_id": 9934930,
              "track_number": "WBILMTESTTRACK",
              "price": 453,
              "rid": "ab4219087a764ae0btest",
              "name": "Mascaras",
              "sale": 30,
              "size": "0",
              "total_price": 317,
              "nm_id": 2389212,
              "brand": "Vivienne Sabo",
              "status": 202
            }
          ],
          "locale": "en",
          "internal_signature": "",
          "customer_id": "test",
          "delivery_service": "meest",
          "shardkey": "9",
          "sm_id": 99,
          "date_created": "2021-11-26T06:22:19Z",
          "oof_shard": "1"
        });

        let response = client
            .post(&url)
            .json(&json_payload)
            .send()
            .await
            .expect("Failed to send request");

        assert_eq!(response.status(), StatusCode::CREATED);
    }
}
