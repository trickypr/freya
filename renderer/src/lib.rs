use dioxus_core::{
    exports::futures_channel::mpsc::UnboundedSender, ElementId, EventPriority, SchedulerMsg,
    UserEvent,
};
use dioxus_html::{
    geometry::{
        euclid::{Length, Point2D},
        Coordinates,
    },
    input_data::{keyboard_types::Modifiers, MouseButton},
    on::{KeyboardData, MouseData},
};
use dioxus_native_core::real_dom::{Node, NodeType, RealDom};
use enumset::enum_set;
use glutin::event::{MouseScrollDelta, WindowEvent};
use layers_engine::Layers;
use layers_engine::{NodeArea, NodeData};
use layout_engine::calculate_node;
use skia_safe::{
    font_style::{Slant, Weight, Width},
    utils::text_utils::Align,
    BlurStyle, Canvas, ClipOp, Font, FontStyle, MaskFilter, Paint, PaintStyle, Path, PathDirection,
    Rect, Typeface,
};
use state::node::{NodeState, SizeMode};
use std::{
    collections::HashMap,
    sync::{mpsc::Receiver, Arc, Mutex},
    thread,
};

use gl::types::*;
use glutin::dpi::PhysicalSize;
use glutin::event::ElementState;
use glutin::window::WindowId;
use glutin::{
    event::{Event, KeyboardInput, VirtualKeyCode},
    event_loop::{ControlFlow, EventLoop},
    window::WindowBuilder,
    GlProfile,
};
use skia_safe::Color;
use skia_safe::{
    gpu::{gl::FramebufferInfo, BackendRenderTarget, SurfaceOrigin},
    ColorType, Surface,
};
use std::ops::Index;

type SkiaDom = Arc<Mutex<RealDom<NodeState>>>;
type EventEmitter = Arc<Mutex<Option<UnboundedSender<SchedulerMsg>>>>;
type WindowedContext = glutin::ContextWrapper<glutin::PossiblyCurrent, glutin::window::Window>;
type RendererRequests = Arc<Mutex<Vec<RendererRequest>>>;

#[derive(Clone, Debug)]
enum RendererRequest {
    MouseEvent {
        name: &'static str,
        event: MouseData,
    },
    #[allow(dead_code)]
    KeyboardEvent {
        name: &'static str,
        event: KeyboardData,
    },
}

pub fn run(skia_dom: SkiaDom, rev_render: Receiver<()>, event_emitter: EventEmitter) {
    let renderer_requests: RendererRequests = Arc::new(Mutex::new(Vec::new()));
    let cursor_pos = Arc::new(Mutex::new((0.0, 0.0)));

    let el = EventLoop::new();

    struct Env {
        surface: Surface,
        gr_context: skia_safe::gpu::DirectContext,
        windowed_context: WindowedContext,
        skia_dom: SkiaDom,
        fb_info: FramebufferInfo,
        renderer_requests: RendererRequests,
        event_emitter: EventEmitter,
        font: Font,
    }

    impl Env {
        pub fn redraw(&mut self) {
            let canvas = self.surface.canvas();
            canvas.clear(Color::WHITE);
            let window_size = self.windowed_context.window().inner_size();
            render(
                &self.skia_dom,
                canvas,
                NodeArea {
                    width: window_size.width as i32,
                    height: window_size.height as i32,
                    x: 0,
                    y: 0,
                },
                self.renderer_requests.clone(),
                &self.event_emitter,
                &self.font,
            );
            self.gr_context.flush(None);
            self.windowed_context.swap_buffers().unwrap();
        }
    }

    let wins = Arc::new(Mutex::new(vec![]));

    let wb = WindowBuilder::new().with_title("test");

    let cb = glutin::ContextBuilder::new()
        .with_depth_buffer(0)
        .with_stencil_buffer(8)
        .with_pixel_format(24, 8)
        .with_gl_profile(GlProfile::Core);

    #[cfg(not(feature = "wayland"))]
    let cb = cb.with_double_buffer(Some(true));

    let windowed_context = cb.build_windowed(wb, &el).unwrap();

    let windowed_context = unsafe { windowed_context.make_current().unwrap() };
    let window_id = windowed_context.window().id();

    gl::load_with(|s| windowed_context.get_proc_address(s));

    let fb_info = {
        let mut fboid: GLint = 0;
        unsafe { gl::GetIntegerv(gl::FRAMEBUFFER_BINDING, &mut fboid) };

        FramebufferInfo {
            fboid: fboid.try_into().unwrap(),
            format: skia_safe::gpu::gl::Format::RGBA8.into(),
        }
    };

    let mut gr_context = skia_safe::gpu::DirectContext::new_gl(None, None).unwrap();

    windowed_context
        .window()
        .set_inner_size(PhysicalSize::<u32>::new(300, 300));

    let mut surface = create_surface(&windowed_context, &fb_info, &mut gr_context);
    let sf = windowed_context.window().scale_factor() as f32;
    surface.canvas().scale((sf, sf));

    let style = FontStyle::new(Weight::NORMAL, Width::NORMAL, Slant::Upright);
    let type_face = Typeface::new("Fira Sans", style).unwrap();
    let font = Font::new(type_face, 16.0);

    let env = Env {
        surface,
        gr_context,
        windowed_context,
        fb_info,
        skia_dom,
        renderer_requests: renderer_requests.clone(),
        event_emitter,
        font,
    };

    wins.lock().unwrap().push(Arc::new(Mutex::new(env)));

    fn create_surface(
        windowed_context: &WindowedContext,
        fb_info: &FramebufferInfo,
        gr_context: &mut skia_safe::gpu::DirectContext,
    ) -> skia_safe::Surface {
        let pixel_format = windowed_context.get_pixel_format();
        let size = windowed_context.window().inner_size();
        let backend_render_target = BackendRenderTarget::new_gl(
            (
                size.width.try_into().unwrap(),
                size.height.try_into().unwrap(),
            ),
            pixel_format.multisampling.map(|s| s.try_into().unwrap()),
            pixel_format.stencil_bits.try_into().unwrap(),
            *fb_info,
        );
        Surface::from_backend_render_target(
            gr_context,
            &backend_render_target,
            SurfaceOrigin::BottomLeft,
            ColorType::RGBA8888,
            None,
            None,
        )
        .unwrap()
    }

    {
        let proxy = el.create_proxy();
        thread::spawn(move || {
            while let Ok(msg) = rev_render.recv() {
                proxy.send_event(msg).unwrap();
            }
        });
    }

    let get_window_context = move |window_id: WindowId| -> Option<Arc<Mutex<Env>>> {
        let mut win = None;
        for env in &*wins.lock().unwrap() {
            if env.lock().unwrap().windowed_context.window().id() == window_id {
                win = Some(env.clone())
            }
        }

        win
    };

    el.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;

        #[allow(deprecated)]
        match event {
            Event::LoopDestroyed => {}
            Event::WindowEvent { event, window_id } => {
                let result = get_window_context(window_id);
                if let Some(env) = result {
                    let env = env.lock().unwrap();
                    env.windowed_context.window().request_redraw();
                }
                match event {
                    WindowEvent::MouseWheel { delta, .. } => {
                        let cursor_pos = cursor_pos.lock().unwrap();
                        let scroll_data = {
                            match delta {
                                MouseScrollDelta::LineDelta(x, y) => (x, y),
                                MouseScrollDelta::PixelDelta(_) => (0.0, 0.0),
                            }
                        };
                        renderer_requests
                            .lock()
                            .unwrap()
                            .push(RendererRequest::MouseEvent {
                                name: "scroll",
                                event: MouseData::new(
                                    Coordinates::new(
                                        Point2D::default(),
                                        Point2D::from_lengths(
                                            Length::new(cursor_pos.0),
                                            Length::new(cursor_pos.1),
                                        ),
                                        Point2D::default(),
                                        Point2D::from_lengths(
                                            Length::new(scroll_data.0 as f64),
                                            Length::new(scroll_data.1 as f64),
                                        ),
                                    ),
                                    Some(MouseButton::Primary),
                                    enum_set! {MouseButton::Primary},
                                    Modifiers::empty(),
                                ),
                            });
                    }
                    WindowEvent::CursorMoved { position, .. } => {
                        let cursor_pos = {
                            let mut cursor_pos = cursor_pos.lock().unwrap();
                            cursor_pos.0 = position.x;
                            cursor_pos.1 = position.y;

                            *cursor_pos
                        };

                        renderer_requests
                            .lock()
                            .unwrap()
                            .push(RendererRequest::MouseEvent {
                                name: "mouseover",
                                event: MouseData::new(
                                    Coordinates::new(
                                        Point2D::default(),
                                        Point2D::from_lengths(
                                            Length::new(cursor_pos.0),
                                            Length::new(cursor_pos.1),
                                        ),
                                        Point2D::default(),
                                        Point2D::default(),
                                    ),
                                    Some(MouseButton::Primary),
                                    enum_set! {MouseButton::Primary},
                                    Modifiers::empty(),
                                ),
                            });
                    }
                    WindowEvent::MouseInput { state, .. } => {
                        if ElementState::Released == state {
                            let cursor_pos = cursor_pos.lock().unwrap();
                            renderer_requests
                                .lock()
                                .unwrap()
                                .push(RendererRequest::MouseEvent {
                                    name: "click",
                                    event: MouseData::new(
                                        Coordinates::new(
                                            Point2D::default(),
                                            Point2D::from_lengths(
                                                Length::new(cursor_pos.0),
                                                Length::new(cursor_pos.1),
                                            ),
                                            Point2D::default(),
                                            Point2D::default(),
                                        ),
                                        Some(MouseButton::Primary),
                                        enum_set! {MouseButton::Primary},
                                        Modifiers::empty(),
                                    ),
                                });
                        }
                    }
                    WindowEvent::Resized(physical_size) => {
                        let result = get_window_context(window_id);
                        if let Some(env) = result {
                            let mut env = env.lock().unwrap();
                            let mut context = env.gr_context.clone();
                            env.surface =
                                create_surface(&env.windowed_context, &env.fb_info, &mut context);
                            env.windowed_context.resize(physical_size)
                        }
                    }
                    WindowEvent::CloseRequested => *control_flow = ControlFlow::Exit,
                    WindowEvent::KeyboardInput {
                        input:
                            KeyboardInput {
                                virtual_keycode,
                                modifiers,
                                ..
                            },
                        ..
                    } => {
                        if modifiers.logo() {
                            if let Some(VirtualKeyCode::Q) = virtual_keycode {
                                *control_flow = ControlFlow::Exit;
                            }
                        }
                    }
                    _ => (),
                }
            }
            Event::RedrawRequested(window_id) => {
                let result = get_window_context(window_id);
                if let Some(env) = result {
                    let mut env = env.lock().unwrap();
                    env.redraw();
                }
            }
            Event::UserEvent(_) => {
                let result = get_window_context(window_id);
                if let Some(env) = result {
                    let mut env = env.lock().unwrap();
                    env.redraw();
                }
            }
            _ => (),
        }
    });
}

fn render_skia(
    dom: &mut &SkiaDom,
    canvas: &mut &mut Canvas,
    node: &NodeData,
    area: &NodeArea,
    font: &Font,
    viewports: &Vec<NodeArea>,
) {
    let node = node.node.as_ref().unwrap();

    match &node.node_type {
        NodeType::Element { tag, children, .. } => {
            match tag.as_str() {
                "view" | "container" => {
                    let mut paint = Paint::default();

                    paint.set_anti_alias(true);
                    paint.set_style(PaintStyle::Fill);
                    paint.set_color(node.state.style.background);

                    let x = area.x;
                    let y = area.y;

                    let x2 = x + area.width;
                    let y2 = y + area.height;

                    //

                    let radius = node.state.style.radius;
                    let radius = if radius < 0 { 0 } else { radius };

                    canvas.save();
                    for viewport in viewports {
                        canvas.clip_rect(
                            Rect::new(
                                viewport.x as f32,
                                viewport.y as f32,
                                (viewport.x + viewport.width) as f32,
                                (viewport.y + viewport.height) as f32,
                            ),
                            ClipOp::Intersect,
                            true,
                        );
                    }

                    let mut path = Path::new();

                    path.add_round_rect(
                        Rect::new(x as f32, y as f32, x2 as f32, y2 as f32),
                        (radius as f32, radius as f32),
                        PathDirection::CW,
                    );

                    path.close();

                    // Shadow effect
                    {
                        let shadow = &node.state.style.shadow;

                        if shadow.intensity > 0 {
                            let mut blur_paint = paint.clone();

                            blur_paint.set_color(shadow.color);
                            blur_paint.set_alpha(shadow.intensity);
                            blur_paint.set_mask_filter(MaskFilter::blur(
                                BlurStyle::Normal,
                                shadow.size,
                                false,
                            ));
                            canvas.draw_path(&path, &blur_paint);
                        }
                    }

                    canvas.draw_path(&path, &paint);

                    canvas.restore();
                }
                "text" => {
                    let mut paint = Paint::default();

                    paint.set_anti_alias(true);
                    paint.set_style(PaintStyle::StrokeAndFill);
                    paint.set_color(Color::WHITE);

                    let child_id = children.get(0);

                    let text = if let Some(child_id) = child_id {
                        let child: Node<NodeState> = {
                            let dom = dom.lock().unwrap();
                            dom.index(*child_id).clone()
                        };

                        if let NodeType::Text { text } = child.node_type {
                            text
                        } else {
                            String::new()
                        }
                    } else {
                        String::new()
                    };

                    let x = area.x;
                    let y = area.y + 12; /* Line height, wip */

                    canvas.draw_str_align(text, (x, y), &font, &paint, Align::Left);
                }
                _ => {}
            }

            #[cfg(feature = "wireframe")]
            {
                let mut path = Path::new();
                let mut paint = Paint::default();

                paint.set_anti_alias(true);
                paint.set_style(PaintStyle::Fill);
                paint.set_color(Color::MAGENTA);

                let x = area.x;
                let y = area.y;

                let x2 = x + area.width;
                let y2 = if area.height < 0 { y } else { y + area.height };

                canvas.draw_line((x as f32, y as f32), (x2 as f32, y as f32), &paint);
                canvas.draw_line((x2 as f32, y as f32), (x2 as f32, y2 as f32), &paint);
                canvas.draw_line((x2 as f32, y2 as f32), (x as f32, y2 as f32), &paint);
                canvas.draw_line((x as f32, y2 as f32), (x as f32, y as f32), &paint);

                path.close();
            }
        }
        _ => {}
    }
}

fn render(
    mut dom: &SkiaDom,
    mut canvas: &mut Canvas,
    area: NodeArea,
    renderer_requests: RendererRequests,
    event_emitter: &EventEmitter,
    font: &Font,
) {
    let root: Node<NodeState> = {
        let dom = dom.lock().unwrap();
        dom.index(ElementId(0)).clone()
    };
    let layers = &mut Layers::default();
    let mut events_filtered: HashMap<&'static str, Vec<(NodeData, RendererRequest)>> =
        HashMap::new();
    calculate_node(
        &NodeData {
            width: SizeMode::Percentage(100),
            height: SizeMode::Percentage(100),
            padding: (0, 0, 0, 0),
            node: Some(root),
        },
        area.clone(),
        area,
        &mut (dom, &mut events_filtered, &renderer_requests),
        layers,
        |node_id, (dom, _, _)| {
            let child = {
                let dom = dom.lock().unwrap();
                dom.index(*node_id).clone()
            };

            Some(NodeData {
                width: child.state.size.width,
                height: child.state.size.height,
                padding: child.state.size.padding,
                node: Some(child),
            })
        },
        0,
    );

    let mut layers_nums: Vec<&i16> = layers.layers.keys().collect();

    // From top to bottom
    layers_nums.sort_by(|a, b| b.cmp(a));

    // Save all the viewports for each layer
    let mut viewports: HashMap<i16, Vec<NodeArea>> = HashMap::new();
    viewports.insert(0, Vec::new());
    let mut viewports_acumulated = Vec::new();

    for layer_num in &layers_nums {
        let layer = layers.layers.get(layer_num).unwrap();

        for element in layer {
            if let NodeType::Element { tag, .. } = &element.node.node.as_ref().unwrap().node_type {
                if tag == "container" {
                    viewports_acumulated.push(element.area.clone());
                }
            }
        }
        // Save all the container's viewports in the next (to bottom) layer
        viewports.insert(**layer_num - 1, viewports_acumulated.clone());
    }

    // From bottom to top
    layers_nums.sort_by(|a, b| a.cmp(b));

    // Render all the layers from the bottom to the top
    for layer_num in &layers_nums {
        let layer = layers.layers.get(layer_num).unwrap();
        for element in layer {
            render_skia(
                &mut dom,
                &mut canvas,
                &element.node,
                &element.area,
                font,
                viewports.get(layer_num).unwrap_or(&Vec::new()),
            );
        }
    }

    layers_nums.sort_by(|a, b| b.cmp(a));

    // Propagate events from the top to the bottom
    for layer_num in &layers_nums {
        let layer = layers.layers.get(layer_num).unwrap();

        for element in layer.iter() {
            let requests = renderer_requests.lock().unwrap();

            for request in requests.iter() {
                let node = &element.node;
                let area = &element.area;
                match request {
                    RendererRequest::MouseEvent { name, event } => {
                        let x = area.x as f64;
                        let y = area.y as f64;
                        let width = (area.x + area.width) as f64;
                        let height = (area.y + area.height) as f64;
                        let cursor = event.client_coordinates();

                        // Make sure the cursor is inside the node area
                        if cursor.x > x && cursor.x < width && cursor.y > y && cursor.y < height {
                            if !events_filtered.contains_key(name) {
                                events_filtered.insert(name, vec![(node.clone(), request.clone())]);
                            } else {
                                events_filtered
                                    .get_mut(name)
                                    .unwrap()
                                    .push((node.clone(), request.clone()));
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    for (event_name, event_nodes) in events_filtered.iter_mut() {
        let dom = dom.lock().unwrap();
        let listeners = dom.get_listening_sorted(event_name);

        let mut found_node: Option<(&Node<NodeState>, &RendererRequest)> = None;

        'event_nodes: for (node_data, request) in event_nodes.iter() {
            let node = node_data.node.as_ref().unwrap();
            if event_name == &"scroll" {
                if node.state.style.background != Color::TRANSPARENT {
                    break 'event_nodes;
                }
                for listener in &listeners {
                    if listener.id == node.id {
                        found_node = Some((node, request));
                    }
                }
            } else {
                for listener in &listeners {
                    if listener.id == node.id {
                        found_node = Some((node, request));

                        break 'event_nodes;
                    }
                }

                // Only let pass the event if the path (from top layer to bottom is transparent)
                if node.state.style.background != Color::TRANSPARENT {
                    break 'event_nodes;
                }
            }
        }

        if let Some((node, request)) = found_node {
            match &request {
                &RendererRequest::MouseEvent { event, .. } => {
                    event_emitter
                        .lock()
                        .unwrap()
                        .as_ref()
                        .unwrap()
                        .unbounded_send(SchedulerMsg::Event(UserEvent {
                            scope_id: None,
                            priority: EventPriority::Medium,
                            element: Some(node.id.clone()),
                            name: event_name,
                            bubbles: false,
                            data: Arc::new(event.clone()),
                        }))
                        .unwrap();
                }
                _ => {}
            }
        }
    }
    renderer_requests.lock().unwrap().clear();
}
