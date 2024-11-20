// extern crate serde;

use axum::{
    http::StatusCode,
    routing::get,
    Json, Router,
};


use serde::Serialize;


pub async fn start_api_server(listener: tokio::net::TcpListener) {
    // initialize tracing
    // tracing_subscriber::fmt::init();

    // build our application with a route
    let app = Router::new()
        // `GET /` goes to `root`
        .route("/", get(root))
        // `POST /users` goes to `create_user`
        .route("/streams", get(list_all_streams));


    axum::serve(listener, app).await.unwrap();
}

// basic handler that responds with a static string
async fn root() -> &'static str {
    "Hello, World!"
}

async fn list_all_streams() -> (StatusCode, Json<Vec<Stream>>) {
    // insert your application logic here

    let stream = Stream {
        id: String::from("1337"),
        stream_type: String::from("RTSP"),
    };

    let stream_list = vec![stream];

    // this will be converted into a JSON response
    // with a status code of `201 Created`
    (StatusCode::OK, Json(stream_list))
}

// the input to our `create_user` handler
#[derive(Serialize)]
struct Stream {
    id: String,
    stream_type: String,
}

// // the output to our `create_user` handler
// #[derive(Serialize)]
// struct User {
//     id: u64,
//     username: String,
// }