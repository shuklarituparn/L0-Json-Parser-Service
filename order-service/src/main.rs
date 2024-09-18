use axum::response::IntoResponse;
use axum::{
    extract::{Json, State},
    http::StatusCode,
    routing::{get, post},
    Router,
};
use clap::Parser;
use lazy_static::lazy_static;
use log::{error, info};
use prometheus::{IntCounter, IntCounterVec, Registry};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::RwLock;
use tokio_postgres::{Client, NoTls};

lazy_static! {      // регистируем метрики для Prometheus
    static ref REGISTRY: Registry = Registry::new();
    static ref ORDER_COUNTER: IntCounter =
        IntCounter::new("orders_total", "Total number of orders").expect("metric can be created");
    static ref DB_REQUEST: IntCounter =
        IntCounter::new("db_requests_total", "Total number of requests to the database").expect("metric can be created");

    static ref ORDER_STATUS: IntCounterVec = IntCounterVec::new(
        prometheus::opts!("order_status", "Status of orders"),
        &["status"]
    )
    .expect("metric can be created");
}

#[derive(Debug, Serialize, Deserialize, Clone)] // структура для представления заказа
struct Order {
    order_uid: String,
    track_number: String,
    entry: String,
    delivery: Delivery,
    payment: Payment,
    items: Vec<Item>,
    locale: String,
    internal_signature: String,
    customer_id: String,
    delivery_service: String,
    shardkey: String,
    sm_id: i64,
    date_created: String,
    oof_shard: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]  // структура для представления данных доставки
struct Delivery {
    name: String,
    phone: String,
    zip: String,
    city: String,
    address: String,
    region: String,
    email: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)] // структура для представления данных платежа
struct Payment {
    transaction: String,
    request_id: String,
    currency: String,
    provider: String,
    amount: i64,
    payment_dt: i64,
    bank: String,
    delivery_cost: i64,
    goods_total: i64,
    custom_fee: i64,
}

#[derive(Debug, Serialize, Deserialize, Clone)] // структура для представления товара
struct Item {
    chrt_id: i64,
    track_number: String,
    price: i64,
    rid: String,
    name: String,
    sale: i64,
    size: String,
    total_price: i64,
    nm_id: i64,
    brand: String,
    status: i64,
}

struct AppState {   // структура состояния приложения
    orders: RwLock<HashMap<String, Order>>, // хранение заказов в кэш
    db_client: Client,    //клиент базы данных
}

#[derive(Parser, Debug)]
#[command(
    author = "rituparn",
    version = "0.1",
    about = "Order service that lets you process the orders as Json"
)]
struct Args {    // cтруктура для парсинга командных аргументов
    #[arg(short, long, default_value = "3000")]
    port: u16,

    #[arg(
        short,
        long,
        default_value = "postgres://user:password@localhost:port/order_service"
    )]
    database_url: String,
}

#[tokio::main]    // основная асинхронная функция приложения
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    tracing_subscriber::fmt::init();

    info!("Starting up");

    REGISTRY.register(Box::new(ORDER_COUNTER.clone())).unwrap();  // регистрация метрик
    REGISTRY.register(Box::new(ORDER_STATUS.clone())).unwrap();
    REGISTRY.register(Box::new(DB_REQUEST.clone())).unwrap();


    // подключение к базе данных
    let (db_client, connection) = tokio_postgres::connect(&args.database_url, NoTls).await?;
    tokio::spawn(async move {
        if let Err(e) = connection.await {
            error!("connection error: {}", e);
        }
    });

    // создание состояния приложения
    let app_state = Arc::new(AppState {
        orders: RwLock::new(HashMap::new()),
        db_client,
    });

    // настройка маршрутов
    let app = Router::new()
        .route("/order", post(create_order))
        .route("/order/:id", get(get_order))
        .route("/metrics", get(metrics))
        .route("/health", get(health_check))
        .with_state(app_state);

    let addr = format!("0.0.0.0:{}", args.port);
    info!("Listening on {}", addr);
    let listener = TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app.into_make_service())
        .await
        .unwrap();

    Ok(())
}

// асинхронная функция для создания заказа
async fn create_order(
    State(state): State<Arc<AppState>>,
    Json(order): Json<Order>,
) -> Result<impl IntoResponse, (StatusCode, String)> {

    // валидация заказа (чтобы не обрабатывать garbage)
    if let Err(e) = validate_order(&order) {
        error!("Invalid order data: {}", e);
        ORDER_STATUS.with_label_values(&["invalid"]).inc();
        return Err((StatusCode::BAD_REQUEST, e));
    }

    // проверка наличия заказа в кэше
    let orders = state.orders.read().await;
    if orders.contains_key(&order.order_uid) {
        error!("Order with UID {} already exists", order.order_uid);
        ORDER_STATUS.with_label_values(&["duplicate"]).inc();
        return Err((StatusCode::CONFLICT, "Order already exists".to_string()));
    }
    drop(orders);

    // сохранение заказа в базе данных
    if let Err(e) = save_order_to_db(&state.db_client, &order).await {
        error!("Failed to save order to database: {}", e);
        ORDER_STATUS.with_label_values(&["db_error"]).inc();
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            "Database error".to_string(),
        ));
    }

    // добавление заказа в кэш после добавления в бд
    let mut orders = state.orders.write().await;
    orders.insert(order.order_uid.clone(), order.clone());
    info!("Created new order with UID: {}", order.order_uid);
    ORDER_COUNTER.inc();
    ORDER_STATUS.with_label_values(&["created"]).inc();

    let success_message = format!("Order with id {} created successfully", order.order_uid);
    Ok((StatusCode::CREATED, Json(success_message)))
}

// асинхронная функция для получения заказа по ID
async fn get_order(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(order_id): axum::extract::Path<String>,
) -> Result<Json<Order>, (StatusCode, String)> {

    // проверка наличия заказа в кэше
    let orders = state.orders.read().await;
    match orders.get(&order_id) {
        Some(order) => {
            info!("Retrieved order with UID: {}", order_id);
            Ok(Json(order.clone()))
        }
        None => match get_order_from_db(&state.db_client, &order_id).await {
            Ok(Some(order)) => {
                info!("Retrieved order with UID {} from database", order_id);
                DB_REQUEST.inc();
                Ok(Json(order))
            }
            Ok(None) => {
                error!("Order with UID {} not found", order_id);
                ORDER_STATUS.with_label_values(&["not_found"]).inc();
                Err((StatusCode::NOT_FOUND, "Order not found".to_string()))
            }
            Err(e) => {
                error!("Database error: {}", e);
                ORDER_STATUS.with_label_values(&["db_error"]).inc();
                Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Database error".to_string(),
                ))
            }
        },
    }
}

// асинхронная функция для получения метрик
async fn metrics() -> Result<String, (StatusCode, String)> {
    use prometheus::Encoder;
    let encoder = prometheus::TextEncoder::new();

    let mut buffer = Vec::new();
    let metric_families = REGISTRY.gather();
    if metric_families.is_empty() {
        error!("No metrics gathered");
    } else {
        info!("Metrics gathered successfully");
    }

    if let Err(e) = encoder.encode(&metric_families, &mut buffer) {
        error!("Could not encode metrics: {}", e);
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            "An unexpected error occurred".to_string(),
        ));
    };

    match String::from_utf8(buffer) {
        Ok(metrics) => Ok(metrics),
        Err(e) => {
            error!("Metrics could not be converted to UTF8: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                "An unexpected error occurred".to_string(),
            ))
        }
    }
}

// функция для валидации данных заказа
fn validate_order(order: &Order) -> Result<(), String> {
    if order.order_uid.is_empty() {
        return Err("order_uid is required".to_string());
    }
    if order.track_number.is_empty() {
        return Err("track_number is required".to_string());
    }
    if order.entry.is_empty() {
        return Err("entry is required".to_string());
    }
    if order.delivery.name.is_empty()
        || order.delivery.phone.is_empty()
        || order.delivery.zip.is_empty()
        || order.delivery.city.is_empty()
        || order.delivery.address.is_empty()
        || order.delivery.region.is_empty()
        || order.delivery.email.is_empty()
    {
        return Err("All delivery fields are required".to_string());
    }
    if order.payment.transaction.is_empty()
        || order.payment.currency.is_empty()
        || order.payment.provider.is_empty()
        || order.payment.amount <= 0
    {
        return Err("All payment fields are required and amount must be positive".to_string());
    }
    if order.items.is_empty() {
        return Err("At least one item is required".to_string());
    }
    for item in &order.items {
        if item.chrt_id <= 0
            || item.price <= 0
            || item.rid.is_empty()
            || item.name.is_empty()
            || item.brand.is_empty()
        {
            return Err(
                "All item fields are required and numeric fields must be positive".to_string(),
            );
        }
    }
    Ok(())
}

// асинхронная функция для сохранения заказа в базе данных
async fn save_order_to_db(client: &Client, order: &Order) -> Result<(), tokio_postgres::Error> {
    let query = "INSERT INTO order_schema.orders (order_uid, order_data) VALUES ($1, $2)";
    let order_data = serde_json::to_string(&order).unwrap();
    client
        .execute(query, &[&order.order_uid, &order_data])
        .await?;

    Ok(())
}

// асинхронная функция для получения заказа из базы данных
async fn get_order_from_db(
    client: &Client,
    order_id: &str,
) -> Result<Option<Order>, tokio_postgres::Error> {
    let query = "SELECT order_data FROM order_schema.orders WHERE order_uid = $1";
    let row = client.query_opt(query, &[&order_id]).await?;
    if let Some(row) = row {
        let order_data: String = row.get(0);
        let order: Order = serde_json::from_str(&order_data).unwrap();
        Ok(Some(order))
    } else {
        Ok(None)
    }
}

// функция для проверки состояния сервиса
async fn health_check() -> &'static str {
    "OK"
}
