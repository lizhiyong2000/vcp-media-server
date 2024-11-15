
use super::api;
use log::{self, info};
pub struct ServiceManager{
    config: Config
}


impl ServiceManager {
    pub fn new(config_path: &str) ->Self{

        let cfg = Config::load(config_path);


        return ServiceManager{config:cfg,};
    }

    pub async fn start_service(&self){

        self.start_api_service("127.0.0.1:3000".to_string()).await;
    }

    async fn start_api_service(&self, addr :String){

        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .unwrap();

        info!("to start api service");

        // info!("to start api service");
        api::start_api_server(listener).await;
    }
}



struct Config{

}

impl  Config {
    pub fn load(config_path: &str) ->Self{

        let cfg = Config{};


        return cfg;
    }

}