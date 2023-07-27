#[derive(Default, Clone, Copy)]
pub(crate) struct Point {
    pub(crate) x: u32,
    pub(crate) y: u32,
}

#[derive(Default, Clone, Copy)]
pub(crate) struct Region {
    pub(crate) x: u32,
    pub(crate) y: u32,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

impl Region {
    fn contains(&self, x: u32, y: u32) -> bool {
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

    pub(crate) fn inverse_scale(&self, inverse_scale: u32) -> Region {
        Region {
            x: self.x / inverse_scale,
            y: self.y / inverse_scale,
            width: self.width / inverse_scale,
            height: self.height / inverse_scale,
        }
    }

    pub(crate) fn scale(&self, scale: u32) -> Region {
        Region {
            x: self.x * scale,
            y: self.y * scale,
            width: self.width * scale,
            height: self.height * scale,
        }
    }
}
