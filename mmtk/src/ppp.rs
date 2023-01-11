use std::sync::Mutex;

use mmtk::util::ObjectReference;

pub struct PPPRegistry {
    ppps: Mutex<Vec<ObjectReference>>,
}

impl PPPRegistry {
    pub fn new() -> Self {
        Self {
            ppps: Mutex::new(Vec::new()),
        }
    }

    pub fn register(&self, object: ObjectReference) {
        let mut ppps = self.ppps.lock().unwrap();
        ppps.push(object);
    }

    pub fn foreach<F>(&self, f: F)
    where
        F: FnMut(ObjectReference),
    {
        let ppps = self.ppps.lock().unwrap();
        ppps.iter().copied().for_each(f);
    }
}
