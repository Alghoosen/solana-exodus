use log::*;
use crossbeam_channel::bounded;
use crossbeam_channel::unbounded;

#[derive(Default, Debug)]
struct ExecutionEnvironment {
    accounts: Vec<i8>,
}

fn main() {
    solana_logger::setup();
    error!("hello");
    let (s, r) = unbounded();

    let mut joins = (0..10).map(|thx| {
        let s = s.clone();
        std::thread::spawn(move || {
            let mut i = 0;
            loop {
                s.send((thx, i, ExecutionEnvironment::default())).unwrap();
                i += 1;
            }
        })
    }).collect::<Vec<_>>();

    joins.push(std::thread::spawn(move || {
        let mut count = 0;
        let start = Instant::now();
        loop {
            r.recv().unwrap();
            count += 1;
            if count % 1_000_000 == 0 {
                error!("recv-ed: {}", count / start.elapsed().as_secs());
            }
        }
    }));
    joins.into_iter().for_each(|j| j.join().unwrap());
}
