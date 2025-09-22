use std::time::Duration;
use glommio::timer::Timer;

pub fn start_routine<F>(mut f: F, duration: Duration) 
where
    F: FnMut() + Send + 'static,
{
    glommio::spawn_local(async move {
        loop {
            Timer::new(duration).await;
            f();
        }
    }).detach();
}
