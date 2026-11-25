use crate::{EdgeVisitable, GcSlot, Header, Heap, HeapObject, ObjectKind, Smi, Visitor};
use core::alloc::Layout;

#[repr(C)]
pub struct Range {
    pub header: Header,
    pub start: GcSlot<Smi>,
    pub end: GcSlot<Smi>,
    pub current: GcSlot<Smi>,
    pub inclusive: GcSlot<Smi>,
}

pub struct RangeInit {
    pub start: i64,
    pub end: i64,
    pub current: i64,
    pub inclusive: bool,
}

impl Range {
    pub fn start(&self) -> i64 {
        self.start.to_smi().value()
    }

    pub fn end(&self) -> i64 {
        self.end.to_smi().value()
    }

    pub fn current(&self) -> i64 {
        self.current.to_smi().value()
    }

    pub fn inclusive(&self) -> bool {
        self.inclusive.to_smi().value() != 0
    }

    pub fn step(&self) -> i64 {
        if self.start() <= self.end() { 1 } else { -1 }
    }

    pub fn has_next(&self) -> bool {
        let current = self.current();
        if self.step() > 0 {
            if self.inclusive() {
                current <= self.end()
            } else {
                current < self.end()
            }
        } else if self.inclusive() {
            current >= self.end()
        } else {
            current > self.end()
        }
    }

    pub fn next_value(&self, heap: &Heap) -> i64 {
        let value = self.current();
        self.current.set(
            heap,
            self.tagged(heap).erase(),
            Smi::new(value.saturating_add(self.step())),
        );
        value
    }

    pub fn count(&self) -> i64 {
        let distance = (i128::from(self.end()) - i128::from(self.start())).abs();
        let count = if self.inclusive() {
            distance + 1
        } else {
            distance
        };
        count.min(i128::from(i64::MAX)) as i64
    }

    pub fn contains(&self, value: i64) -> bool {
        if self.step() > 0 {
            value >= self.start() && value < self.end() || self.inclusive() && value == self.end()
        } else {
            value <= self.start() && value > self.end() || self.inclusive() && value == self.end()
        }
    }
}

impl HeapObject for Range {
    const KIND: ObjectKind = ObjectKind::Range;
    type Init<'a> = RangeInit;

    fn layout_for(_config: &Self::Init<'_>) -> Layout {
        Layout::new::<Self>()
    }

    fn init(&mut self, heap: &Heap, config: &Self::Init<'_>) {
        self.header.map.init(heap.known().range_map.get(heap));
        let host = self.tagged(heap).erase();
        self.start.set(heap, host, Smi::new(config.start));
        self.end.set(heap, host, Smi::new(config.end));
        self.current.set(heap, host, Smi::new(config.current));
        self.inclusive
            .set(heap, host, Smi::new(if config.inclusive { 1 } else { 0 }));
    }

    fn header(&self) -> &Header {
        &self.header
    }

    fn layout(&self) -> Layout {
        Layout::new::<Self>()
    }
}

impl EdgeVisitable for Range {
    fn visit_edges(&self, visitor: &mut dyn Visitor) {
        visitor.visit(self.header.map.as_raw());
        visitor.visit(self.start.as_raw());
        visitor.visit(self.end.as_raw());
        visitor.visit(self.current.as_raw());
        visitor.visit(self.inclusive.as_raw());
    }
}
