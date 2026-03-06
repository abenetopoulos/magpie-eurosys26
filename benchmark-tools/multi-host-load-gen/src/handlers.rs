use actix_web::{get, post, web, HttpResponse, Responder};

use chrono;
use load_gen_api;
use tokio;

use crate::experiment::ExperimentRunner;
use crate::Args;

#[post("/init_experiment")]
pub async fn init_experiment_handler(
    experiment_setup: web::Json<load_gen_api::ExperimentSetup>,
) -> impl Responder {
    let args = Args::from(&*experiment_setup);
    let experiment_runner = ExperimentRunner::get_experiment_runner_mut();
    let handle = tokio::runtime::Handle::current();
    let _ = tokio::task::spawn_blocking(move || {
        experiment_runner.init_experiment(args, &experiment_setup.target_hosts, &handle);
    })
    .await;
    HttpResponse::Ok()
}

#[post("/start_experiment_at_deadline")]
pub async fn start_experiment_at_deadline_handler(
    experiment_deadline: web::Json<load_gen_api::ExperimentDeadline>,
) -> impl Responder {
    let current_time = chrono::offset::Utc::now();
    let time_to_sleep = (experiment_deadline.deadline - current_time)
        .to_std()
        .unwrap();
    tokio::task::spawn(async move {
        let _ = tokio::task::spawn_blocking(move || {
            std::thread::sleep(time_to_sleep);
            let experiment_runner = ExperimentRunner::get_experiment_runner_mut();
            experiment_runner.start_experiment(true);
        })
        .await;
    });

    HttpResponse::Ok()
}

#[get("/experiment_finished")]
pub async fn get_experiment_finished_handler() -> impl Responder {
    let experiment_runner = ExperimentRunner::get_experiment_runner();
    HttpResponse::Ok().json(&load_gen_api::ExperimentFinished {
        finished: experiment_runner.is_completed(),
    })
}
