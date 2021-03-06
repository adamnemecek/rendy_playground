//!
//! adapted from the rendy meshes demo
//!

#![cfg_attr(
    not(any(feature = "dx12", feature = "metal", feature = "vulkan")),
    allow(unused)
)]

// #[cfg(feature = "dx12")]
// use gfx_backend_dx12::Backend;

// #[cfg(feature = "metal")]
// use gfx_backend_metal::Backend;

// #[cfg(feature = "vulkan")]
use gfx_backend_vulkan::Backend;
use rand::prelude::*;
use rendy::shader::SpirvReflection;
use rendy_playground::crystal;
use std::sync::mpsc::{channel, sync_channel, Receiver, Sender};
use {
    genmesh::generators::{IndexedPolygon, SharedVertex},
    rand::distributions::{Distribution, Uniform},
    rendy::{
        command::{DrawIndexedCommand, QueueId, RenderPassEncoder},
        factory::{Config, Factory},
        graph::{render::*, GraphBuilder, GraphContext, NodeBuffer, NodeImage},
        hal::{self, adapter::PhysicalDevice as _, device::Device as _},
        init::winit::{
            event::{Event, WindowEvent},
            event_loop::{ControlFlow, EventLoop},
            window::WindowBuilder,
        },
        init::AnyWindowedRendy,
        memory::Dynamic,
        mesh::{Mesh, Model, PosColorNorm},
        resource::{Buffer, BufferInfo, DescriptorSet, DescriptorSetLayout, Escape, Handle},
        shader::{ShaderKind, SourceLanguage, SourceShaderInfo, SpirvShader},
    },
    std::{cmp::min, mem::size_of, time},
};
use {
    genmesh::Triangulate, nalgebra::Vector3, random_color::RandomColor, rendy::mesh::Position,
    rendy_playground::player,
};

lazy_static::lazy_static! {
    static ref VERTEX: SpirvShader = SourceShaderInfo::new(
        include_str!("shader.vert"),
        concat!(env!("CARGO_MANIFEST_DIR"), "/examples/meshes_simple/shader.vert").into(),
        ShaderKind::Vertex,
        SourceLanguage::GLSL,
        "main",
    ).precompile().unwrap();

    static ref FRAGMENT: SpirvShader = SourceShaderInfo::new(
        include_str!("shader.frag"),
        concat!(env!("CARGO_MANIFEST_DIR"), "/examples/meshes_simple/shader.frag").into(),
        ShaderKind::Fragment,
        SourceLanguage::GLSL,
        "main",
    ).precompile().unwrap();

    static ref SHADERS: rendy::shader::ShaderSetBuilder = rendy::shader::ShaderSetBuilder::default()
        .with_vertex(&*VERTEX).unwrap()
        .with_fragment(&*FRAGMENT).unwrap();

    static ref SHADER_REFLECTION: SpirvReflection = SHADERS.reflect().unwrap();
}

#[derive(Clone, Copy)]
#[repr(C, align(16))]
struct UniformArgs {
    proj: nalgebra::Matrix4<f32>,
    view: nalgebra::Matrix4<f32>,
    model: [nalgebra::Matrix4<f32>; 6],
}

#[derive(Clone, Copy, Debug)]
#[repr(C, align(16))]
struct PerInstanceConst {
    translate: nalgebra::Vector3<f32>,
    dir: u32,
}

#[derive(Clone, Copy, Debug)]
#[repr(C, align(16))]
struct PerInstance {
    color: nalgebra::Vector3<f32>,
    pad: u32,
}

#[derive(Debug)]
struct Camera {
    view: nalgebra::Projective3<f32>,
    // proj: nalgebra::Perspective3<f32>,
    proj: nalgebra::Matrix4<f32>,
}

struct Scene<B: hal::Backend> {
    camera: Camera,
    object_mesh: Option<Mesh<B>>,
    per_instance_const: Vec<PerInstanceConst>,
    per_instance: Vec<PerInstance>,
}

const UNIFORM_SIZE: u64 = size_of::<UniformArgs>() as u64;
const NUM_INSTANCES: u64 = 1024 * 1024;
const PER_INSTANCE_CONST_SIZE: u64 = size_of::<PerInstanceConst>() as u64;
const PER_INSTANCE_SIZE: u64 = size_of::<PerInstance>() as u64;

const fn align_to(s: u64, align: u64) -> u64 {
    ((s - 1) / align + 1) * align
}
const fn buffer_const_size(align: u64) -> u64 {
    align_to(PER_INSTANCE_CONST_SIZE * NUM_INSTANCES, align)
}
const fn buffer_frame_size(align: u64) -> u64 {
    align_to(UNIFORM_SIZE + PER_INSTANCE_SIZE * NUM_INSTANCES, align)
}
const fn buffer_size(align: u64, frames: u64) -> u64 {
    buffer_const_size(align) + buffer_frame_size(align) * frames
}
const fn uniform_offset(index: usize, align: u64) -> u64 {
    buffer_const_size(align) + buffer_frame_size(align) * index as u64
}
const fn per_instance_offset(index: usize, align: u64) -> u64 {
    uniform_offset(index, align) + UNIFORM_SIZE
}

#[derive(Debug, Default)]
struct MeshRenderPipelineDesc;

#[derive(Debug)]
struct MeshRenderPipeline<B: hal::Backend> {
    align: u64,
    buffer: Escape<Buffer<B>>,
    sets: Vec<Escape<DescriptorSet<B>>>,
}

struct ProfileTimer {
    label: std::string::String,
    start: std::time::Instant,
}

impl ProfileTimer {
    fn start(label: &str) -> Self {
        ProfileTimer {
            label: label.into(),
            start: std::time::Instant::now(),
        }
    }
}

// impl Drop for ProfileTimer {
//     fn drop(&mut self) {
//         println!("{}: {:?}", self.label, self.start.elapsed());
//     }
// }

impl<B> SimpleGraphicsPipelineDesc<B, Scene<B>> for MeshRenderPipelineDesc
where
    B: hal::Backend,
{
    type Pipeline = MeshRenderPipeline<B>;

    fn load_shader_set(
        &self,
        factory: &mut Factory<B>,
        _scene: &Scene<B>,
    ) -> rendy_shader::ShaderSet<B> {
        SHADERS.build(factory, Default::default()).unwrap()
    }

    fn vertices(
        &self,
    ) -> Vec<(
        Vec<hal::pso::Element<hal::format::Format>>,
        hal::pso::ElemStride,
        hal::pso::VertexInputRate,
    )> {
        return vec![
            SHADER_REFLECTION
                .attributes(&["position"])
                .unwrap()
                .gfx_vertex_input_desc(hal::pso::VertexInputRate::Vertex),
            SHADER_REFLECTION
                .attributes(&["translate", "dir"])
                .unwrap()
                .gfx_vertex_input_desc(hal::pso::VertexInputRate::Instance(1)),
            SHADER_REFLECTION
                .attributes(&["color", "pad"])
                .unwrap()
                .gfx_vertex_input_desc(hal::pso::VertexInputRate::Instance(1)),
        ];
    }

    fn layout(&self) -> Layout {
        return SHADER_REFLECTION.layout().unwrap();
    }

    fn build<'a>(
        self,
        ctx: &GraphContext<B>,
        factory: &mut Factory<B>,
        _queue: QueueId,
        scene: &Scene<B>,
        buffers: Vec<NodeBuffer>,
        images: Vec<NodeImage>,
        set_layouts: &[Handle<DescriptorSetLayout<B>>],
    ) -> Result<MeshRenderPipeline<B>, rendy_core::hal::pso::CreationError> {
        assert!(buffers.is_empty());
        assert!(images.is_empty());
        assert_eq!(set_layouts.len(), 1);

        let frames = ctx.frames_in_flight as _;
        let align = factory
            .physical()
            .limits()
            .min_uniform_buffer_offset_alignment;

        let mut buffer = factory
            .create_buffer(
                BufferInfo {
                    size: buffer_size(align, frames) as u64,
                    usage: hal::buffer::Usage::UNIFORM
                        | hal::buffer::Usage::INDIRECT
                        | hal::buffer::Usage::VERTEX,
                },
                Dynamic,
            )
            .unwrap();

        let mut sets = Vec::new();
        for index in 0..frames {
            unsafe {
                let set = factory
                    .create_descriptor_set(set_layouts[0].clone())
                    .unwrap();
                factory.write_descriptor_sets(Some(hal::pso::DescriptorSetWrite {
                    set: set.raw(),
                    binding: 0,
                    array_offset: 0,
                    descriptors: Some(hal::pso::Descriptor::Buffer(
                        buffer.raw(),
                        Some(uniform_offset(index as usize, align))
                            ..Some(uniform_offset(index as usize, align) + UNIFORM_SIZE),
                    )),
                }));
                sets.push(set);
            }
        }

        if !scene.per_instance_const.is_empty() {
            // println!(
            //     "upload const: {}",
            //     std::mem::size_of::<PerInstanceConst>() * scene.per_instance_const.len()
            // );
            unsafe {
                factory
                    .upload_visible_buffer(&mut buffer, 0, &scene.per_instance_const[..])
                    .expect("update const buffer failed")
            };
        }

        Ok(MeshRenderPipeline {
            align,
            buffer,
            sets,
        })
    }
}

fn model_transform() -> nalgebra::Matrix4<f32> {
    let rot = nalgebra::UnitQuaternion::identity();
    nalgebra::Similarity3::from_parts(Vector3::new(0.5, 0.5, 0.0).into(), rot, 0.5).into()
}

fn model_transform2() -> [nalgebra::Matrix4<f32>; 6] {
    let z_pos = nalgebra::UnitQuaternion::identity();
    let z_neg = nalgebra::UnitQuaternion::face_towards(
        &Vector3::new(0.0, 0.0, -1.0),
        &Vector3::new(0.0, 1.0, 0.0),
    );
    let x_pos = nalgebra::UnitQuaternion::face_towards(
        &Vector3::new(1.0, 0.0, 0.0),
        &Vector3::new(0.0, 1.0, 0.0),
    );
    let x_neg = nalgebra::UnitQuaternion::face_towards(
        &Vector3::new(-1.0, 0.0, 0.0),
        &Vector3::new(0.0, 1.0, 0.0),
    );
    let y_pos = nalgebra::UnitQuaternion::face_towards(
        &Vector3::new(0.0, 1.0, 0.0),
        &Vector3::new(0.0, 0.0, 1.0),
    );
    let y_neg = nalgebra::UnitQuaternion::face_towards(
        &Vector3::new(0.0, -1.0, 0.0),
        &Vector3::new(0.0, 0.0, -1.0),
    );
    // let unit = 0.125;
    let unit = 0.125;
    let scale = 0.125;
    [
        nalgebra::Similarity3::from_parts(Vector3::new(0.0, 0.0, unit).into(), z_pos, scale).into(),
        nalgebra::Similarity3::from_parts(Vector3::new(0.0, 0.0, -unit).into(), z_neg, scale)
            .into(),
        nalgebra::Similarity3::from_parts(Vector3::new(unit, 0.0, 0.0).into(), x_pos, scale).into(),
        nalgebra::Similarity3::from_parts(Vector3::new(-unit, 0.0, 0.0).into(), x_neg, scale)
            .into(),
        nalgebra::Similarity3::from_parts(Vector3::new(0.0, unit, 0.0).into(), y_pos, scale).into(),
        nalgebra::Similarity3::from_parts(Vector3::new(0.0, -unit, 0.0).into(), y_neg, scale)
            .into(),
    ]
}

impl<B> SimpleGraphicsPipeline<B, Scene<B>> for MeshRenderPipeline<B>
where
    B: hal::Backend,
{
    type Desc = MeshRenderPipelineDesc;

    fn prepare(
        &mut self,
        factory: &Factory<B>,
        _queue: QueueId,
        _set_layouts: &[Handle<DescriptorSetLayout<B>>],
        index: usize,
        scene: &Scene<B>,
    ) -> PrepareResult {
        let pt = ProfileTimer::start("prepare");

        // println!("index: {}", index);

        // println!(
        //     "upload uniform {}: {}",
        //     index,
        //     std::mem::size_of::<UniformArgs>()
        // );
        unsafe {
            factory
                .upload_visible_buffer(
                    &mut self.buffer,
                    uniform_offset(index, self.align),
                    &[UniformArgs {
                        // proj: scene.camera.proj.to_homogeneous(),
                        proj: scene.camera.proj,
                        view: scene.camera.view.to_homogeneous(),
                        model: model_transform2(),
                    }],
                )
                .unwrap()
        };
        // {
        //     let per_instance = &scene.per_instance[..];
        //     println!(
        //         "upload dyn {}: {}",
        //         index,
        //         // std::mem::size_of::<PerInstance>() * scene.per_instance.len(),
        //         std::mem::size_of_val(per_instance)
        //     );
        // }
        if !scene.per_instance.is_empty() {
            unsafe {
                factory
                    .upload_visible_buffer(
                        &mut self.buffer,
                        per_instance_offset(index, self.align),
                        &scene.per_instance[..],
                    )
                    .unwrap()
            };
        }
        PrepareResult::DrawReuse
    }

    fn draw(
        &mut self,
        layout: &B::PipelineLayout,
        mut encoder: RenderPassEncoder<'_, B>,
        index: usize,
        scene: &Scene<B>,
    ) {
        println!("draw");

        unsafe {
            encoder.bind_graphics_descriptor_sets(
                layout,
                0,
                Some(self.sets[index].raw()),
                std::iter::empty(),
            );

            let vertex = [SHADER_REFLECTION.attributes(&["position"]).unwrap()];

            scene
                .object_mesh
                .as_ref()
                .unwrap()
                .bind(0, &vertex, &mut encoder)
                .unwrap();
            encoder.bind_vertex_buffers(1, std::iter::once((self.buffer.raw(), 0)));
            encoder.bind_vertex_buffers(
                2,
                std::iter::once((self.buffer.raw(), per_instance_offset(index, self.align))),
            );
            encoder.draw_indexed(
                0..scene.object_mesh.as_ref().unwrap().len(),
                0 as i32,
                0..scene.per_instance.len() as u32,
            )
        }
    }

    fn dispose(self, _factory: &mut Factory<B>, _scene: &Scene<B>) {}
}

fn main() {
    env_logger::Builder::from_default_env()
        .filter_module("meshes", log::LevelFilter::Trace)
        .init();

    let mut event_loop = EventLoop::new();

    let window = WindowBuilder::new()
        .with_inner_size((960, 640).into())
        .with_title("Rendy example");

    let config: Config = Default::default();
    let rendy = AnyWindowedRendy::init_auto(&config, window, &event_loop).unwrap();

    rendy::with_any_windowed_rendy!((rendy)
        use back; (mut factory, mut families, surface, window) => {

        let mut graph_builder = GraphBuilder::<Backend, Scene<Backend>>::new();

        let size = window.inner_size().to_physical(window.hidpi_factor());
        let window_kind = hal::image::Kind::D2(size.width as u32, size.height as u32, 1, 1);
        let aspect = size.width / size.height;

        let depth = graph_builder.create_image(
            window_kind,
            1,
            hal::format::Format::D32Sfloat,
            Some(hal::command::ClearValue {
                depth_stencil: hal::command::ClearDepthStencil {
                    depth: 1.0,
                    stencil: 0,
                },
            }),
        );

        let pass = graph_builder.add_node(
            MeshRenderPipeline::builder()
                .into_subpass()
                .with_color_surface()
                .with_depth_stencil(depth)
                .into_pass()
                .with_surface(
                    surface,
                    hal::window::Extent2D {
                        width: size.width as _,
                        height: size.height as _,
                    },
                    Some(hal::command::ClearValue {
                        color: hal::command::ClearColor {
                            float32: [0.5, 0.5, 1.0, 1.0],
                        },
                    }),
                ),
        );

        let bm = crystal::read_map("hidden_ramp.txt").expect("could not read file");

        let mut planes = crystal::PlanesSep::new();
        planes.create_planes(&bm);
        let planes_copy : Vec<crystal::Plane> = planes.planes_iter().cloned().collect();

        let mut scene = Scene {
            camera: Camera {
                proj: nalgebra::Perspective3::new(aspect as f32, 3.1415 / 4.0, 1.0, 200.0)
                    .to_homogeneous(),
                view: nalgebra::Projective3::identity() * nalgebra::Translation3::new(0.0, 0.0, 10.0),
            },
            object_mesh: None,
            per_instance: vec![],
            per_instance_const: vec![],
        };

        // let mut rng = rand::thread_rng();
        // let col_dist = Uniform::new(0.5, 1.0);

        let mut rc = RandomColor::new();
        rc.luminosity(random_color::Luminosity::Bright);
        println!("planes: {}", planes_copy.len());
        for i in 0..std::cmp::min(NUM_INSTANCES as usize,planes_copy.len()) {
            let color = rc.to_rgb_array();
            let point = planes_copy[i].cell;
            let dir = match planes_copy[i].dir {
                crystal::Dir::ZxPos => 4,
                crystal::Dir::ZxNeg => 5,
                crystal::Dir::YzPos => 2,
                crystal::Dir::YzNeg => 3,
                crystal::Dir::XyPos => 0,
                crystal::Dir::XyNeg => 1,
            };
            scene.per_instance_const.push(PerInstanceConst{
                translate: nalgebra::Vector3::new(point[0] as f32 * 0.25, point[1] as f32 * 0.25, point[2] as f32 * 0.25),
                dir : dir,
            });
            scene.per_instance.push(PerInstance{
                color : nalgebra::Vector3::new(
                    color[0] as f32 / 255.0,
                    color[1] as f32 / 255.0,
                    color[2] as f32 / 255.0,
                ),
                pad : 0,
            });
        }

        let graph = graph_builder
        .build(&mut factory, &mut families, &scene)
        .unwrap();

        // let icosphere = genmesh::generators::IcoSphere::subdivide(3);
        // let icosphere = genmesh::generators::Torus::new(1f32, 0.5f32, 32, 32);
        let icosphere = genmesh::generators::Plane::new();
        // icosphere.
        let indices: Vec<_> =
            genmesh::Vertices::vertices(icosphere.indexed_polygon_iter().triangulate())
                .map(|i| i as u32)
                .collect();

        println!("indices: {}", indices.len());
        let vertices: Vec<_> = icosphere
            .shared_vertex_iter()
            .map(|v| Position(v.pos.into()))
            .collect();
        println!("vertices: {}", vertices.len());
        for v in &vertices {
            println!("vert: {:?}", v);
        }
        scene.object_mesh = Some(
            Mesh::<Backend>::builder()
                .with_indices(&indices[..])
                .with_vertices(&vertices[..])
                .build(graph.node_queue(pass), &factory)
                .unwrap(),
        );

        let started = time::Instant::now();

        let mut frames = 0u64..;
        // let rxy = Uniform::new(-1.0, 1.0);
        // let rz = Uniform::new(0.0, 185.0);

        let mut checkpoint = started;
        let mut player_state = player::State::new();
        let mut event_manager = player::EventManager::new();
        let mut graph = Some(graph);
        event_loop.run(move |event, _, control_flow| {
            *control_flow = ControlFlow::Poll;
            match event {
                Event::WindowEvent { event, .. } => match event {
                    WindowEvent::CloseRequested => *control_flow = ControlFlow::Exit,
                    _ => event_manager.window_event(event)
                },
                Event::EventsCleared => {
                    if event_manager.should_close() {
                        *control_flow = ControlFlow::Exit;
                    }
                    factory.maintain(&mut families);


                    player_state.apply_events(event_manager.input_events());
                    scene.camera = Camera {
                        // proj: nalgebra::Perspective3::new(aspect as f32, 3.1415 / 4.0, 1.0, 200.0),
                        proj: rendy_playground::math::perspective_projection(
                            aspect as f32,
                            3.1415 / 4.0,
                            1.0,
                            200.0,
                        ),
                        view: player_state.get_view_matrix(),
                    };

                    if let Some(ref mut graph) = graph {
                        let pt = ProfileTimer::start("graph.run");
                        graph.run(&mut factory, &mut families, &scene);
                    }

                    let elapsed = checkpoint.elapsed();
                    if (checkpoint.elapsed() >= std::time::Duration::from_secs(5))
                    {
                        checkpoint = time::Instant::now();
                        // let mut rng = thread_rng();
                        // let scene = &mut scene.rad_scene;
                        // for i in 0..scene.planes.num_planes() {
                        //     // seriously, there is no Vec.fill?
                        //     scene.diffuse[i] = Vector3::new(1f32, 1f32, 1f32);
                        //     scene.emit[i] = Vector3::new(0.0, 0.0, 0.0);
                        // }
                        // let mut rc = RandomColor::new();
                        // rc.luminosity(random_color::Luminosity::Bright);
                        // let num_dots = 1000;
                        // for _ in 0..num_dots {
                        //     let i = rng.gen_range(0, scene.planes.num_planes());
                        //     let color = rc.to_rgb_array();
                        //     scene.emit[i] = Vector3::new(color[0] as f32 / 255.0, color[1] as f32 / 255.0,color[2] as f32 / 255.0,);
                        // }
                    }
                    // {
                    //     let pt= ProfileTimer::start("rad");
                    //     scene.rad_scene.do_rad();
                    // }
                    // for i in 0..scene.rad_scene.planes.num_planes() {
                    //     scene.per_instance[i].color[0] = scene.rad_scene.rad_front.r[i];
                    //     scene.per_instance[i].color[1] = scene.rad_scene.rad_front.g[i];
                    //     scene.per_instance[i].color[2] = scene.rad_scene.rad_front.b[i];
                    // }
                    // for pi in &mut scene.per_instance {
                    //     let color = rc.to_rgb_array();
                    //     pi.color = nalgebra::Vector3::new(
                    //         color[0] as f32 / 255.0,
                    //         color[1] as f32 / 255.0,
                    //         color[2] as f32 / 255.0,
                    //     );
                    // }
                }
                _ => {}
            }
            if *control_flow == ControlFlow::Exit {
                if let Some(graph) = graph.take() {
                    graph.dispose(&mut factory, &scene);
                }
                drop(scene.object_mesh.take());
            }
        });
    });
}
