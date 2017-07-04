
extern crate webrender;
extern crate gleam;
extern crate stylish;
extern crate app_units;
extern crate stb_truetype;
extern crate euclid;

mod assets;
pub use assets::*;
mod math;
mod color;
use color::*;
mod shadow;
use shadow::*;
mod layout;
mod border;
mod filter;

use webrender::*;
use webrender_api::*;
use std::error::Error;
use std::collections::HashMap;
use std::rc::Rc;
use std::cell::RefCell;

// TODO: Don't box errors, error chain would be better
type WResult<T> = Result<T, Box<Error>>;

/// Allows for rendering a `stylish::Manager` via webrender.
///
/// # Supported Properties
///
/// * `background_color` - Set the color of the bounds
///                        of this element.
///
///    Possible values:
///
///    * `"#RRGGBB"` - **R**ed, **G**reen, **B**lue in hex.
///    * `"#RRGGBBAA"` - **R**ed, **G**reen, **B**lue, **A**lpha
///                       in hex.
///    * `rgb(R, G, B)` - **R**ed, **G**reen, **B**lue in decimal 0-255.
///    * `rgba(R, G, B, A)` - **R**ed, **G**reen, **B**lue, **A**lpha
///                        in decimal 0-255.
pub struct WebRenderer<A> {
    assets: Rc<A>,
    renderer: Renderer,
    api: RenderApi,
    frame_id: Epoch,

    images: HashMap<String, ImageKey>,
    fonts: FontMap,

    skip_build: bool,
}

type FontMap = Rc<RefCell<HashMap<String, Font>>>;

struct Font {
    key: FontKey,
    info: stb_truetype::FontInfo<Vec<u8>>,
}

impl <A: Assets + 'static> WebRenderer<A> {
    pub fn new<F>(
        load_fn: F,
        assets: A,
        manager: &mut stylish::Manager<Info>,
    ) -> WResult<WebRenderer<A>>
        where F: Fn(&str) -> *const ()
    {
        let gl = unsafe {gleam::gl::GlFns::load_with(|f|
            load_fn(f) as *const _
        )};

        manager.add_func_raw("rgb", rgb);
        manager.add_func_raw("rgba", rgba);
        manager.add_func_raw("gradient", gradient);
        manager.add_func_raw("stop", stop);
        manager.add_func_raw("deg", math::deg);
        manager.add_func_raw("shadow", shadow);
        manager.add_func_raw("shadows", shadows);
        manager.add_func_raw("border", border::border);
        manager.add_func_raw("bside", border::border_side);
        manager.add_func_raw("border_width", border::border_width);
        manager.add_func_raw("border_image", border::border_image);
        manager.add_func_raw("filters", filter::filters);

        let fonts = Rc::new(RefCell::new(HashMap::new()));
        let assets = Rc::new(assets);

        let options = webrender::RendererOptions {
            device_pixel_ratio: 1.0,
            resource_override_path: None,
            debug: false,
            clear_framebuffer: false,
            .. Default::default()
        };
        let (renderer, sender) = webrender::Renderer::new(gl, options, DeviceUintSize::new(800, 480)).unwrap();
        let api = sender.create_api();
        renderer.set_render_notifier(Box::new(Dummy));

        let pipeline = PipelineId(0, 0);
        api.set_root_pipeline(pipeline);

        {
            let fonts = fonts.clone();
            let sender = sender.clone();
            let assets = assets.clone();
            manager.add_layout_engine("lined", move |obj| {
                Box::new(layout::Lined::new(
                    obj,
                    sender.create_api(),
                    fonts.clone(),
                    assets.clone(),
                ))
            });
        }
        manager.add_layout_engine("grid", |obj| Box::new(layout::Grid::new(obj)));

        Ok(WebRenderer {
            assets: assets,
            renderer: renderer,
            api: api,
            frame_id: Epoch(0),

            images: HashMap::new(),
            fonts: fonts,
            skip_build: false,
        })
    }

    pub fn layout(&mut self, manager: &mut stylish::Manager<Info>, width: u32, height: u32) {
        if manager.layout(width as i32, height as i32) {
            self.skip_build = false;
        } else {
            self.skip_build = true;
        }
    }

    pub fn render(&mut self, manager: &mut stylish::Manager<Info>, width: u32, height: u32) {
        self.frame_id.0 += 1;
        let pipeline = PipelineId(0, 0);
        self.renderer.update();
        let size = DeviceUintSize::new(width, height);
        let dsize = LayoutSize::new(width as f32, height as f32);

        if !self.skip_build {
            let mut builder = DisplayListBuilder::new(
                pipeline,
                dsize
            );

            manager.render(&mut WebBuilder {
                api: &self.api,
                builder: &mut builder,
                assets: self.assets.clone(),
                images: &mut self.images,
                fonts: self.fonts.clone(),
                offset: Vec::with_capacity(16),
            });

            self.api.set_window_parameters(
                size,
                DeviceUintRect::new(
                    DeviceUintPoint::zero(),
                    size,
                )
            );
            self.api.set_display_list(
                None,
                self.frame_id,
                dsize,
                builder.finalize(),
                false,
            );
            self.api.generate_frame(None);
        }

        self.renderer.render(size);
        self.skip_build = false;
    }
}

#[derive(Debug)]
pub struct Info {
    background_color: Option<Color>,
    image: Option<ImageKey>,
    shadows: Vec<Shadow>,

    text: Option<Text>,

    border_widths: BorderWidths,
    border: Option<BorderDetails>,

    clip_id: Option<ClipId>,
    clip_overflow: bool,

    scroll_offset: LayoutVector2D,
    filters: Vec<FilterOp>,
}

#[derive(Debug)]
struct Text {
    glyphs: Vec<GlyphInstance>,
    font: FontKey,
    size: i32,
    color: ColorF,
}

struct WebBuilder<'a, A: 'a> {
    api: &'a RenderApi,
    builder: &'a mut DisplayListBuilder,

    assets: Rc<A>,
    images: &'a mut HashMap<String, ImageKey>,
    fonts: FontMap,

    offset: Vec<LayoutPoint>,
}

impl <'a, A: Assets> stylish::RenderVisitor<Info> for WebBuilder<'a, A> {
    fn visit(&mut self, obj: &mut stylish::RenderObject<Info>) {
        use std::collections::hash_map::Entry;

        let width = obj.draw_rect.width as f32;
        let height = obj.draw_rect.height as f32;

        let offset = self.offset.last().cloned().unwrap_or(LayoutPoint::zero());

        let rect = LayoutRect::new(
            LayoutPoint::new(obj.draw_rect.x as f32 + offset.x, obj.draw_rect.y as f32 + offset.y),
            LayoutSize::new(width, height),
        );

        if obj.render_info.is_none() {
            let text = if let (Some(txt), Some(font)) = (obj.text.as_ref(), obj.get_value::<String>("font")) {
                let mut fonts = self.fonts.borrow_mut();
                let finfo = match fonts.entry(font) {
                    Entry::Occupied(v) => Some(v.into_mut()),
                    Entry::Vacant(v) => {
                        if let Some(data) = self.assets.load_font(v.key()) {
                            let info = stb_truetype::FontInfo::new(data.clone(), 0).unwrap();
                            let key = self.api.generate_font_key();
                            self.api.add_raw_font(key, data, 0);
                            Some(v.insert(Font {
                                key: key,
                                info: info,
                            }))
                        } else { None }
                    },
                };
                if let Some(finfo) = finfo {
                    let size = obj.get_value::<i32>("font_size").unwrap_or(16);
                    let color = if let Some(Color::Solid(col)) = Color::get(obj, "font_color") {
                        col
                    } else {
                        ColorF::new(0.0, 0.0, 0.0, 1.0)
                    };

                    if obj.text_splits.is_empty() {
                        obj.text_splits.push((0, txt.len(), obj.draw_rect));
                    }

                    let scale = finfo.info.scale_for_pixel_height(size as f32);
                    let glyphs = obj.text_splits.iter()
                        .flat_map(|&(s, e, rect)| {
                            let rect = rect;
                            let finfo = &finfo;
                            txt[s..e].chars()
                                .scan((0.0, None), move |state, v| {
                                    let index = finfo.info.find_glyph_index(v as u32);
                                    let g_size = if let Some(last) = state.1 {
                                        let kern = finfo.info.get_glyph_kern_advance(last, index);
                                        kern as f32 * scale
                                    } else {
                                        0.0
                                    };
                                    state.1 = Some(index);

                                    let pos = state.0 + g_size;
                                    state.0 += g_size + finfo.info.get_glyph_h_metrics(index).advance_width as f32 * scale;

                                    Some(GlyphInstance {
                                        index: index,
                                        point: LayoutPoint::new(
                                            rect.x as f32 + offset.x + pos,
                                            rect.y as f32 + offset.y + size as f32 * 0.8,
                                        ),
                                    })
                                })
                        })
                        .collect();
                    Some(Text {
                        glyphs: glyphs,
                        font: finfo.key,
                        size: size,
                        color: color,
                    })
                } else {
                    None
                }
            } else {
                None
            };

            let mut load_image = |v| match self.images.entry(v) {
                    Entry::Occupied(v) => Some(*v.get()),
                    Entry::Vacant(v) => {
                        if let Some(img) = self.assets.load_image(v.key()) {
                            let key = self.api.generate_image_key();
                            self.api.add_image(
                                key,
                                ImageDescriptor {
                                    format: match img.components {
                                        Components::RGB => ImageFormat::RGB8,
                                        Components::BGRA => ImageFormat::BGRA8,
                                    },
                                    width: img.width,
                                    height: img.height,
                                    stride: None,
                                    offset: 0,
                                    is_opaque: img.is_opaque,
                                },
                                ImageData::new(img.data),
                                None
                            );
                            Some(*v.insert(key))
                        } else {
                            None
                        }
                    },
                };

            obj.render_info = Some(Info {
                background_color: Color::get(obj, "background_color"),
                image: obj.get_value::<String>("image")
                    .and_then(|v| load_image(v)),
                shadows: obj.get_custom_value::<Shadow>("shadow")
                    .cloned()
                    .map(|v| vec![v])
                    .or_else(|| obj.get_custom_value::<Vec<Shadow>>("shadow")
                        .cloned())
                    .unwrap_or_else(Vec::new),
                text: text,

                border_widths: obj.get_custom_value::<border::BorderWidthInfo>("border_width")
                    .map(|v| v.widths)
                    .unwrap_or(BorderWidths {
                        left: 0.0,
                        top: 0.0,
                        right: 0.0,
                        bottom: 0.0,
                    }),
                border: obj.get_custom_value::<border::Border>("border")
                    .map(|v| match *v {
                        border::Border::Normal{left, top, right, bottom} => BorderDetails::Normal(NormalBorder {
                            left: left,
                            top: top,
                            right: right,
                            bottom: bottom,

                            radius: BorderRadius::uniform(obj.get_value::<f64>("border_radius").unwrap_or(0.0) as f32),
                        }),
                        border::Border::Image{ref image, patch, repeat, fill} => BorderDetails::Image(ImageBorder {
                            image_key: load_image(image.clone()).unwrap(),
                            patch: patch,
                            fill: fill,
                            outset: euclid::SideOffsets2D::new(0.0, 0.0, 0.0, 0.0),
                            repeat_horizontal: repeat,
                            repeat_vertical: repeat,
                        }),
                    }),

                clip_id: None,
                clip_overflow: obj.get_value::<bool>("clip_overflow").unwrap_or(false),
                scroll_offset: LayoutVector2D::new(
                    obj.get_value::<f64>("scroll_x").unwrap_or(0.0) as f32,
                    obj.get_value::<f64>("scroll_y").unwrap_or(0.0) as f32,
                ),

                filters: obj.get_custom_value::<filter::Filters>("filters")
                    .map(|v| v.0.clone())
                    .unwrap_or_default(),
            });
        }

        let info = obj.render_info.as_mut().unwrap();

        if !info.filters.is_empty() {
            self.builder.push_stacking_context(
                ScrollPolicy::Scrollable,
                LayoutRect::new(
                    LayoutPoint::zero(),
                    LayoutSize::zero(),
                ),
                None,
                TransformStyle::Flat,
                None,
                MixBlendMode::Normal,
                info.filters.clone(),
            );
        }

        if let Some(key) = info.image {
            self.builder.push_image(rect, rect, rect.size, LayoutSize::zero(), ImageRendering::Auto, key);

        }

        if let Some(col) = info.background_color.as_ref() {
            match *col {
                Color::Solid(col) => {
                    self.builder.push_rect(rect, rect, col);
                },
                Color::Gradient{angle, ref stops} => {
                    let len = width.max(height) / 2.0;
                    let x = len * angle.cos();
                    let y = len * angle.sin();

                    let g = self.builder.create_gradient(
                        LayoutPoint::new(width / 2.0 - x, height / 2.0 - y),
                        LayoutPoint::new(width / 2.0 + x, height / 2.0 + y),
                        stops.clone(),
                        ExtendMode::Clamp,
                    );
                    self.builder.push_gradient(
                        rect, rect,
                        g,
                        LayoutSize::new(width, height),
                        LayoutSize::zero(),
                    );
                }
            }
        }

        if let Some(border) = info.border {
            self.builder.push_border(
                rect,
                rect,
                info.border_widths,
                border,
            );
        }

        if let Some(txt) = info.text.as_ref() {
            self.builder.push_text(
                rect,
                rect,
                &txt.glyphs,
                txt.font,
                txt.color,
                app_units::Au::from_f64_px(txt.size as f64 * 0.8),
                0.0,
                None
            );
        }

        for shadow in &info.shadows {
            let clip = self.builder.push_clip_region(
                &rect.inflate(shadow.blur_radius, shadow.blur_radius)
                    .translate(&shadow.offset),
                None, None
            );
            self.builder.push_box_shadow(
                rect,
                clip,
                rect,
                shadow.offset,
                shadow.color,
                shadow.blur_radius,
                shadow.spread_radius,
                0.0,
                shadow.clip_mode,
            );
        }

        info.clip_id = if info.clip_overflow {
            let clip = self.builder.push_clip_region(&rect, None, None);
            let id = self.builder.define_clip(rect, clip, None);
            self.builder.push_clip_id(id);
            Some(id)
        } else {
            None
        };

        self.offset.push(rect.origin + info.scroll_offset);
    }

    fn visit_end(&mut self, obj: &mut stylish::RenderObject<Info>) {
        let info = obj.render_info.as_mut().unwrap();
        if let Some(_clip_id) = info.clip_id {
            self.builder.pop_clip_id();
        }
        if !info.filters.is_empty() {
            self.builder.pop_stacking_context();
        }
        self.offset.pop();
    }
}

struct Dummy;
impl RenderNotifier for Dummy {
    fn new_frame_ready(&mut self) {
    }

    fn new_scroll_frame_ready(&mut self, _composite_needed: bool) {
    }
}