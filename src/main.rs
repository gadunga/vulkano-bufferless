#[macro_use]
extern crate vulkano;
extern crate vulkano_shaders;
extern crate winit;
extern crate vulkano_win;

use vulkano_win::VkSurfaceBuild;

use vulkano::command_buffer::{AutoCommandBufferBuilder, DynamicState};
use vulkano::device::{Device, DeviceExtensions};
use vulkano::framebuffer::{Framebuffer, Subpass};
use vulkano::instance::{Instance, PhysicalDevice};
use vulkano::pipeline::{GraphicsPipeline, vertex::BufferlessVertices, viewport::Viewport};
use vulkano::swapchain;
use vulkano::swapchain::{
    PresentMode, SurfaceTransform, Swapchain, 
    AcquireError, SwapchainCreationError
};
use vulkano::sync::{FlushError, GpuFuture, now};

use winit::{Event, EventsLoop, WindowBuilder, WindowEvent};

use std::sync::Arc;

fn main() {
    let instance = {
        let extensions = vulkano_win::required_extensions();
        Instance::new(None, &extensions, None)
            .expect("failed to create Vulkan instance")
    };

    let physical = PhysicalDevice::enumerate(&instance)
        .next()
        .expect("no device available");
    println!("Using device: {} (type: {:?})", physical.name(), physical.ty());

    let mut events_loop = EventsLoop::new();
    let surface = WindowBuilder::new()
        .build_vk_surface(&events_loop, instance.clone())
        .unwrap();
    let queue_family = physical
        .queue_families()
        .find(|&q| {
            q.supports_graphics() && surface.is_supported(q).unwrap_or(false)
        })
        .expect("couldn't find a graphical queue family");

    let (device, mut queues) = {
        let device_ext = DeviceExtensions {
            khr_swapchain: true,
            .. DeviceExtensions::none()
        };

        Device::new(physical, physical.supported_features(), &device_ext,
                    [(queue_family, 0.5)].iter().cloned())
            .expect("failed to create device")
    };

    let queue = queues.next().unwrap();
    let mut dimensions;
    let (mut swapchain, mut images) = {
        let caps = surface
            .capabilities(physical)
            .expect("failed to get surface capabilities");

        dimensions = caps.current_extent.unwrap_or([1024, 768]);
        let alpha = caps.supported_composite_alpha.iter().next().unwrap();
        let format = caps.supported_formats[0].0;

        Swapchain::new(device.clone(), surface.clone(), caps.min_image_count, format,
                       dimensions, 1, caps.supported_usage_flags, &queue,
                       SurfaceTransform::Identity, alpha, PresentMode::Fifo, true,
                       None)
            .expect("failed to create swapchain")
    };
    
    //Note the vertex shader takes no inputs and instead uses gl_VertexIndex
    //to create verticies
    mod vs {
        vulkano_shaders::shader!{
            ty: "vertex",
            src: "
#version 450

layout(location = 0) out vec2 v_screen_coords;

void main() {
    v_screen_coords = vec2((gl_VertexIndex << 1) & 2, gl_VertexIndex & 2);
	gl_Position = vec4(v_screen_coords * 2.0f + -1.0f, 0.0f, 1.0f);
}
"
        }
    }

    mod fs {
        vulkano_shaders::shader!{
            ty: "fragment",
            src: "
#version 450

layout(location = 0) in vec2 v_screen_coords;
layout(location = 0) out vec4 f_color;

void main() {
    f_color = vec4(v_screen_coords, 0.0, 1.0);
}
"
        }
    }

    let vs = vs::Shader::load(device.clone()).expect("failed to create shader module");
    let fs = fs::Shader::load(device.clone()).expect("failed to create shader module");
    let render_pass = Arc::new(single_pass_renderpass!(device.clone(),
        attachments: {
            color: {
                load: Clear,
                store: Store,
                format: swapchain.format(),
                samples: 1,
            }
        },
        pass: {
            color: [color],
            depth_stencil: {}
        }
    ).unwrap());

    //GraphicsPipelineBuilder defaults to buffer-less vertex inputs as of 0.7.2
    let pipeline = Arc::new(GraphicsPipeline::start()
        .cull_mode_front()
        .front_face_counter_clockwise()
        .vertex_shader(vs.main_entry_point(), ())
        .viewports_dynamic_scissors_irrelevant(1)
        .fragment_shader(fs.main_entry_point(), ())
        .render_pass(Subpass::from(render_pass.clone(), 0).unwrap())
        .build(device.clone())
        .unwrap());

    let mut framebuffers: Option<Vec<Arc<vulkano::framebuffer::Framebuffer<_,_>>>> = None;
    let mut recreate_swapchain = false;
    let mut previous_frame_end = Box::new(now(device.clone())) as Box<GpuFuture>;

    let mut dynamic_state = DynamicState {
        line_width: None,
        viewports: Some(vec![Viewport {
            origin: [0.0, 0.0],
            dimensions: [dimensions[0] as f32, dimensions[1] as f32],
            depth_range: 0.0 .. 1.0,
        }]),
        scissors: None,
    };

    loop {
        previous_frame_end.cleanup_finished();

        if recreate_swapchain {
            dimensions = surface
                .capabilities(physical)
                .expect("failed to get surface capabilities")
                .current_extent
                .unwrap();

            let (new_swapchain, new_images) = 
                match swapchain.recreate_with_dimension(dimensions) {
                    Ok(r) => r,
                    Err(SwapchainCreationError::UnsupportedDimensions) => {
                        continue;
                    },
                    Err(err) => panic!("{:?}", err)
                };

            swapchain = new_swapchain;
            images = new_images;

            framebuffers = None;

            dynamic_state.viewports = Some(vec![Viewport {
                origin: [0.0, 0.0],
                dimensions: [dimensions[0] as f32, dimensions[1] as f32],
                depth_range: 0.0 .. 1.0,
            }]);

            recreate_swapchain = false;
        }

        if framebuffers.is_none() {
            framebuffers = Some(images.iter().map(|image| {
                Arc::new(Framebuffer::start(render_pass.clone())
                    .add(image.clone())
                    .unwrap()
                    .build()
                    .unwrap())
            }).collect::<Vec<_>>());
        }

        let (image_num, acquire_future) = 
            match swapchain::acquire_next_image(swapchain.clone(),  None) {
                Ok(r) => r,
                Err(AcquireError::OutOfDate) => {
                    recreate_swapchain = true;
                    continue;
                },
                Err(err) => panic!("{:?}", err)
            };

        let command_buffer = 
            AutoCommandBufferBuilder::primary_one_time_submit(device.clone(), queue.family())
                .unwrap()
                .begin_render_pass(framebuffers.as_ref().unwrap()[image_num].clone(), 
                    false, vec![[0.0, 0.0, 1.0, 1.0].into()])
                .unwrap()
                .draw(pipeline.clone(),
                    &dynamic_state,
                    BufferlessVertices{ vertices: 3, instances: 1 }, //Here's where the magic happens
                    (), 
                    ())
                .unwrap()
                .end_render_pass()
                .unwrap()
                .build()
                .unwrap();

        let future = previous_frame_end
            .join(acquire_future)
            .then_execute(queue.clone(), command_buffer)
            .unwrap()
            .then_swapchain_present(queue.clone(), swapchain.clone(), image_num)
            .then_signal_fence_and_flush();

        match future {
            Ok(future) => {
                previous_frame_end = Box::new(future) as Box<_>;
            }
            Err(FlushError::OutOfDate) => {
                recreate_swapchain = true;
                previous_frame_end = Box::new(now(device.clone())) as Box<_>;
            }
            Err(e) => {
                println!("{:?}", e);
                previous_frame_end = Box::new(now(device.clone())) as Box<_>;
            }
        }
        
        let mut done = false;
        events_loop.poll_events(|ev| {
            match ev {
                Event::WindowEvent { event: WindowEvent::CloseRequested, .. } => done = true,
                _ => ()
            }
        });
        if done { return; }
    }
}