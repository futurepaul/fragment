use druid::kurbo::{Affine, BezPath, Point, Rect, Size};

use druid::piet::{
    Color, FontBuilder, ImageFormat, InterpolationMode, RenderContext, Text, TextLayoutBuilder,
};

use druid::shell::{runloop, WindowBuilder};

use druid::widget::{Column, TextBox, Label, Scroll, Padding};

use druid::{
    Action, BaseState, BoxConstraints, Env, Event, EventCtx, LayoutCtx, PaintCtx, UiMain, UiState,
    UpdateCtx, Widget, WidgetPod, Data
};

use fragment_search::{search, ListItem};

struct ConstrainedBox<T: Data> {
    // constraints: BoxConstraints,
    child: WidgetPod<T, Box<dyn Widget<T>>>,
}

impl<T: Data> ConstrainedBox<T> {
    pub fn new(child: impl Widget<T> + 'static) -> ConstrainedBox<T> {
        ConstrainedBox {
            child: WidgetPod::new(child).boxed()
        }
    }
}

impl<T: Data> Widget<T> for ConstrainedBox<T> {
fn paint(
        &mut self,
        paint_ctx: &mut PaintCtx,
        base_state: &BaseState,
        data: &T,
        env: &Env,
    ) {
        let dbg_rect = Rect::from_origin_size(Point::ORIGIN, base_state.size());
        paint_ctx.fill(dbg_rect, &Color::rgba8(0x00, 0x00, 0xff, 0x33));
        self.child.paint(paint_ctx, data, env);
    }

    fn layout(
        &mut self,
        layout_ctx: &mut LayoutCtx,
        bc: &BoxConstraints,
        data: &T,
        env: &Env,
    ) -> Size {
        let new_bc = BoxConstraints::tight(bc.max());
        dbg!(new_bc);
        let size = self.child.layout(layout_ctx, &new_bc, data, env);
        let mut my_size = bc.max();
        if bc.max().width == std::f64::INFINITY {
            my_size.width = bc.constrain(size).width;
        }
        if bc.max().height == std::f64::INFINITY {
            my_size.height = bc.constrain(size).height;
        }
        // let size = self.child.layout(layout_ctx, &new_bc, data, env);
        dbg!(size);
        bc.constrain(my_size)
    }

    fn event(
        &mut self,
        _event: &Event,
        _ctx: &mut EventCtx,
        _data: &mut T,
        _env: &Env,
    ) -> Option<Action> {
        None
    }

    fn update(
        &mut self,
        _ctx: &mut UpdateCtx,
        _old_data: Option<&T>,
        _data: &T,
        _env: &Env,
    ) {
    }
}

struct SearchResults {
    results: Vec<Label>
}

impl Widget<String> for SearchResults {
    // The paint method gets called last, after an event flow.
    // It goes event -> update -> layout -> paint, and each method can influence the next.
    // Basically, anything that changes the appearance of a widget causes a paint.
    fn paint(
        &mut self,
        paint_ctx: &mut PaintCtx,
        base_state: &BaseState,
        data: &String,
        _env: &Env,
    ) {
        // Let's draw a picture with Piet!
        // Clear the whole context with the color of your choice
        paint_ctx.clear(Color::WHITE);

        // Create an arbitrary bezier path
        // (base_state.size() returns the size of the layout rect we're painting in)
        let mut path = BezPath::new();
        path.move_to(Point::ORIGIN);
        path.quad_to(
            (80.0, 90.0),
            (base_state.size().width, base_state.size().height),
        );
        // Create a color
        let stroke_color = Color::rgb8(0x00, 0x80, 0x00);
        // Stroke the path with thickness 1.0
        paint_ctx.stroke(path, &stroke_color, 1.0);

        // Rectangles: the path for practical people
        let rect = Rect::from_origin_size((10., 10.), (100., 100.));
        // Note the Color:rgba8 which includes an alpha channel (7F in this case)
        let fill_color = Color::rgba8(0x00, 0x00, 0x00, 0x7F);
        paint_ctx.fill(rect, &fill_color);

        // Text is easy, if you ignore all these unwraps. Just pick a font and a size.
        let font = paint_ctx
            .text()
            .new_font_by_name("Segoe UI", 24.0)
            .unwrap()
            .build()
            .unwrap();
        // Here's where we actually use the UI state
        let layout = paint_ctx
            .text()
            .new_text_layout(&font, data)
            .unwrap()
            .build()
            .unwrap();

        // Let's rotate our text slightly. First we save our current (default) context:
        paint_ctx
            .with_save(|rc| {
                // Now we can rotate the context (or set a clip path, for instance):
                rc.transform(Affine::rotate(0.1));
                rc.draw_text(&layout, (80.0, 40.0), &fill_color);
                Ok(())
            })
            .unwrap();
        // When we exit with_save, the original context's rotation is restored

        // Let's burn some CPU to make a (partially transparent) image buffer
        let image_data = make_image_data(256, 256);
        let image = paint_ctx
            .make_image(256, 256, &image_data, ImageFormat::RgbaSeparate)
            .unwrap();
        // The image is automatically scaled to fit the rect you pass to draw_image
        paint_ctx.draw_image(
            &image,
            Rect::from_origin_size(Point::ORIGIN, base_state.size()),
            InterpolationMode::Bilinear,
        );
    }

    fn layout(
        &mut self,
        _layout_ctx: &mut LayoutCtx,
        bc: &BoxConstraints,
        _data: &String,
        _env: &Env,
    ) -> Size {
        // You can return any Size.
        // Flexible widgets are based on the BoxConstraints passed by their parent widget.
        bc.max()
    }

    fn event(
        &mut self,
        _event: &Event,
        _ctx: &mut EventCtx,
        _data: &mut String,
        _env: &Env,
    ) -> Option<Action> {
        None
    }

    fn update(
        &mut self,
        _ctx: &mut UpdateCtx,
        _old_data: Option<&String>,
        _data: &String,
        _env: &Env,
    ) {
    }
}

fn main() {
    druid::shell::init();

    let search_result = search("what", "../notes").expect("search didn't work");

    let mut run_loop = runloop::RunLoop::new();
    let mut builder = WindowBuilder::new();
    let mut root = Column::new();
    let textbox = TextBox::new();

    root.add_child(Padding::uniform(5.0, textbox), 0.0);

    let mut list = Column::new();

    for item in search_result {
        dbg!(&item.first_line);
        list.add_child(Label::new(item.file_name), 1.0);
    }

    root.add_child(Padding::uniform(5.0, Scroll::new(list)), 1.0);
    
    let state = UiState::new(root, "Druid + Piet".to_string());

    builder.set_title("Custom widget example");
    builder.set_handler(Box::new(UiMain::new(state)));
    let window = builder.build().unwrap();
    window.show();
    run_loop.run();
}

fn make_image_data(width: usize, height: usize) -> Vec<u8> {
    let mut result = vec![0; width * height * 4];
    for y in 0..height {
        for x in 0..width {
            let ix = (y * width + x) * 4;
            result[ix + 0] = x as u8;
            result[ix + 1] = y as u8;
            result[ix + 2] = !(x as u8);
            result[ix + 3] = 127;
        }
    }
    result
}