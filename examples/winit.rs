#![windows_subsystem = "windows"]

use std::mem::transmute;
use std::time::Instant;

use imgui::{Context, FontConfig, FontSource};
use imgui_winit_support::{HiDpiMode, WinitPlatform};
use windows::core::Interface;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use winit::dpi::LogicalSize;
use winit::event::{Event, WindowEvent};
use winit::event_loop::EventLoop;
use winit::platform::windows::*;
use winit::window::WindowBuilder;

use imgui_dx11_renderer::Renderer;

const WINDOW_WIDTH: f64 = 760.0;
const WINDOW_HEIGHT: f64 = 760.0;

type Result<T> = windows::core::Result<T>;

fn d3d11_initialize(
    window: windows::Win32::Foundation::HWND,
) -> Result<(ID3D11Device, IDXGISwapChain, ID3D11DeviceContext)> {
    //Device options
    let drivertype = D3D_DRIVER_TYPE_HARDWARE;
    let mut flags = D3D11_CREATE_DEVICE_BGRA_SUPPORT;
    if true {
        flags |= D3D11_CREATE_DEVICE_DEBUG;
    }
    let feature_levels = [D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_10_0];
    let mut feature_level = D3D_FEATURE_LEVEL_11_1;

    //Swapchain options
    let sc_desc = DXGI_SWAP_CHAIN_DESC {
        BufferDesc: DXGI_MODE_DESC {
            Width: 0,
            Height: 0,
            RefreshRate: DXGI_RATIONAL { Numerator: 60, Denominator: 1 },
            Format: DXGI_FORMAT_R8G8B8A8_UNORM,
            ..Default::default()
        },
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
        BufferCount: 3,
        OutputWindow: window,
        Windowed: true.into(),
        SwapEffect: DXGI_SWAP_EFFECT_DISCARD,
        Flags: DXGI_SWAP_CHAIN_FLAG_ALLOW_MODE_SWITCH.0 as u32,
    };

    //Results
    let mut device = None;
    let mut swapchain = None;
    let mut device_context = None;

    unsafe {
        D3D11CreateDeviceAndSwapChain(
            None,
            drivertype,
            None,
            flags,
            Some(&feature_levels),
            D3D11_SDK_VERSION,
            Some(&sc_desc),
            Some(&mut swapchain),
            Some(&mut device),
            Some(&mut feature_level),
            Some(&mut device_context),
        )?;
}

    Ok((device.unwrap(), swapchain.unwrap(), device_context.unwrap()))
}

fn create_render_target(
    swapchain: &IDXGISwapChain,
    device: &ID3D11Device,
) -> Result<ID3D11RenderTargetView> {
    unsafe {
        let backbuffer: ID3D11Resource = swapchain.GetBuffer(0)?;
        let mut render_target = None;
        device.CreateRenderTargetView(&backbuffer, None, Some(&mut render_target))?;
        Ok(render_target.unwrap())
    }
}

fn main() -> Result<()> {
    let event_loop = EventLoop::new();
    let window = WindowBuilder::new()
        .with_title("imgui_dx11_renderer winit example")
        .with_inner_size(LogicalSize { width: WINDOW_WIDTH, height: WINDOW_HEIGHT })
        .build(&event_loop)
        .unwrap();

    let (device, swapchain, device_ctx) = d3d11_initialize(HWND(window.hwnd()))?;

    let mut target = Some(create_render_target(&swapchain, &device)?);

    let mut imgui = Context::create();
    let mut platform = WinitPlatform::init(&mut imgui);
    imgui.set_ini_filename(None);
    platform.attach_window(imgui.io_mut(), &window, HiDpiMode::Locked(1.0));

    let hidpi_factor = window.scale_factor();
    let font_size = (13.0 * hidpi_factor) as f32;
    imgui.fonts().add_font(&[FontSource::DefaultFontData {
        config: Some(FontConfig { size_pixels: font_size, ..FontConfig::default() }),
    }]);

    let mut renderer = Renderer::new(&mut imgui, &device)?;
    let mut last_frame = Instant::now();

    event_loop.run(move |event, _, control_flow| match event {
        Event::NewEvents(_) => {
            let now = Instant::now();
            imgui.io_mut().update_delta_time(now - last_frame);
            last_frame = now;
        },
        Event::MainEventsCleared => {
            let io = imgui.io_mut();
            platform.prepare_frame(io, &window).expect("Failed to start frame");
            window.request_redraw();
        },
        Event::RedrawRequested(_) => {
            unsafe {
                device_ctx.OMSetRenderTargets(Some(&[target.clone().unwrap()]), None);
                device_ctx.ClearRenderTargetView(
                    target.as_ref().unwrap(),
                    &[0.0 as f32, 0.0, 1.0, 1.0] as *const f32,
                );
            }
            let ui = imgui.frame();
            ui.window("Hello world").size([300.0, 100.0], imgui::Condition::FirstUseEver).build(
                || {
                    ui.text("Hello world!");
                    ui.text("This...is...imgui-rs!");
                    ui.separator();
                    let mouse_pos = ui.io().mouse_pos;
                    ui.text(format!("Mouse Position: ({:.1},{:.1})", mouse_pos[0], mouse_pos[1]));
                },
            );
            ui.show_demo_window(&mut true);

            platform.prepare_render(&ui, &window);
            renderer.render(imgui.render()).unwrap();
            unsafe {
                swapchain.Present(1, 0).unwrap();
            }
        },
        Event::WindowEvent { event: WindowEvent::CloseRequested, .. } => {
            *control_flow = winit::event_loop::ControlFlow::Exit
        },
        Event::WindowEvent {
            event: WindowEvent::Resized(winit::dpi::PhysicalSize { height, width }),
            ..
        } => {
            target = None;
            unsafe {
                swapchain.ResizeBuffers(0, width, height, DXGI_FORMAT_UNKNOWN, 0).unwrap();
            }
            target = create_render_target(&swapchain, &device).ok();
            platform.handle_event(imgui.io_mut(), &window, &event);
        },
        Event::LoopDestroyed => (),
        event => platform.handle_event(imgui.io_mut(), &window, &event),
    })
}
