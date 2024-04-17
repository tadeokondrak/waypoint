#[derive(Default, Clone, Copy, Debug)]
pub(crate) struct Point {
    pub(crate) x: i32,
    pub(crate) y: i32,
}

#[derive(Default, Clone, Copy, Debug)]
pub(crate) struct Region {
    pub(crate) x: i32,
    pub(crate) y: i32,
    pub(crate) width: i32,
    pub(crate) height: i32,
}

impl Region {
    fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && x < self.x + self.width && y >= self.y && y < self.y + self.height
    }

    pub(crate) fn center(self) -> Point {
        Point {
            x: self.x + self.width / 2,
            y: self.y + self.height / 2,
        }
    }

    pub(crate) fn cut_up(mut self) -> Region {
        self.height /= 2;
        self
    }

    pub(crate) fn cut_down(mut self) -> Region {
        self.height /= 2;
        self.y += self.height;
        self
    }

    pub(crate) fn cut_left(mut self) -> Region {
        self.width /= 2;
        self
    }

    pub(crate) fn cut_right(mut self) -> Region {
        self.width /= 2;
        self.x += self.width;
        self
    }

    pub(crate) fn move_up(mut self) -> Region {
        self.y = self.y.saturating_sub(self.height);
        self
    }

    pub(crate) fn move_down(mut self) -> Region {
        self.y = self.y.saturating_add(self.height);
        self
    }

    pub(crate) fn move_left(mut self) -> Region {
        self.x = self.x.saturating_sub(self.width);
        self
    }

    pub(crate) fn move_right(mut self) -> Region {
        self.x = self.x.saturating_add(self.width);
        self
    }

    pub(crate) fn contains_region(&self, other: &Region) -> bool {
        self.contains(other.x, other.y)
            && self.contains(other.x + other.width - 1, other.y + other.height - 1)
    }

    pub(crate) fn scale(&self, scale: u32) -> Region {
        Region {
            x: self.x * scale as i32,
            y: self.y * scale as i32,
            width: self.width * scale as i32,
            height: self.height * scale as i32,
        }
    }

    pub(crate) fn union(&self, other: &Region) -> Region {
        let left = self.x.min(other.x);
        let top = self.y.min(other.y);
        let right = self.right().max(other.right());
        let bottom = self.bottom().max(other.bottom());
        Region {
            x: left,
            y: top,
            width: right - left,
            height: bottom - top,
        }
    }

    pub(crate) fn right(&self) -> i32 {
        self.x + self.width
    }

    pub(crate) fn bottom(&self) -> i32 {
        self.y + self.height
    }
}
