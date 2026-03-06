use async_std::sync::{Arc, Condvar, Mutex, RwLock};
use num_derive::FromPrimitive;
use object_lib::ObjectId;

#[derive(Copy, Clone, FromPrimitive)]
pub enum ObjectMoveStatus {
    NotStarted = 0,
    InProgress,
    Completed,
    Denied,
}

#[derive(Clone)]
pub struct ObjectMoveHandle {
    pub object_id: ObjectId,
    // FIXME if we end up using this in the future, it should be turned into an FSM
    pub move_status: Arc<RwLock<ObjectMoveStatus>>,

    move_done: Arc<Mutex<bool>>,
    move_done_cv: Arc<Condvar>,
}

impl ObjectMoveHandle {
    pub fn new(object_id: ObjectId, status: Option<ObjectMoveStatus>) -> Self {
        let move_status = match status {
            // TODO we should check if `status` corresponds to a completed status (for whatever
            // reason)
            Some(s) => s,
            None => ObjectMoveStatus::NotStarted,
        };

        Self {
            object_id,
            move_status: Arc::new(RwLock::new(move_status)),
            move_done: Arc::new(Mutex::new(false)),
            move_done_cv: Arc::new(Condvar::new()),
        }
    }

    pub async fn set_status_in_progress(&self) {
        match *self.move_status.read().await {
            ObjectMoveStatus::NotStarted => {
                let mut move_status = self.move_status.write().await;
                *move_status = ObjectMoveStatus::InProgress;
            }
            _ => (),
        };
    }

    pub async fn mark_move_done(&self) {
        let mut move_done = self.move_done.lock().await;
        {
            let mut move_status = self.move_status.write().await;
            *move_status = ObjectMoveStatus::Completed;
        }
        *move_done = true;
        self.move_done_cv.notify_all();
    }

    pub async fn mark_move_denied(&self) {
        let mut move_done = self.move_done.lock().await;
        {
            let mut move_status = self.move_status.write().await;
            *move_status = ObjectMoveStatus::Denied;
        }
        *move_done = true;
        self.move_done_cv.notify_all();
    }

    // TODO change return type to Result<>
    pub async fn wait_until_done(&self) {
        let mut move_done = self.move_done.lock().await;
        while !*move_done {
            move_done = self.move_done_cv.wait(move_done).await;
        }

        let move_status = self.move_status.read().await;
        if let ObjectMoveStatus::Completed = *move_status {
            return;
        }
        todo!("wait_until_done: move done but not successful");
    }
}
