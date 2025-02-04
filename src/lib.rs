#![cfg(windows)]
#![deny(missing_docs)]
#![no_std]
//! This crate offers a DirectX 11 renderer for the [imgui-rs](https://docs.rs/imgui/*/imgui/) rust bindings.

use alloc::string::ToString;
use alloc::vec::Vec;
use core::{mem, slice};

use imgui::internal::RawWrapper;
use imgui::{
    BackendFlags, DrawCmd, DrawCmdParams, DrawData, DrawIdx, DrawVert, TextureId, Textures,
};
use windows::core::*;
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;

type Result<T> = windows::core::Result<T>;

const FONT_TEX_ID: usize = !0;

const VERTEX_BUF_ADD_CAPACITY: usize = 5000;
const INDEX_BUF_ADD_CAPACITY: usize = 10000;

#[repr(C)]
struct VertexConstantBuffer {
    mvp: [[f32; 4]; 4],
}

/// A DirectX 11 renderer for (Imgui-rs)[https://docs.rs/imgui/*/imgui/].
#[derive(Debug)]
pub struct Renderer {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    vertex_shader: ID3D11VertexShader,
    pixel_shader: ID3D11PixelShader,
    input_layout: ID3D11InputLayout,
    constant_buffer: ID3D11Buffer,
    blend_state: ID3D11BlendState,
    rasterizer_state: ID3D11RasterizerState,
    depth_stencil_state: ID3D11DepthStencilState,
    font_resource_view: ID3D11ShaderResourceView,
    font_sampler: ID3D11SamplerState,
    vertex_buffer: Buffer,
    index_buffer: Buffer,
    textures: Textures<ID3D11ShaderResourceView>,
}

impl Renderer {
    /// Creates a new renderer for the given [`ID3D11Device`].
    ///
    /// # Safety
    ///
    /// `device` must be a valid [`ID3D11Device`] pointer.
    ///
    /// [`ID3D11Device`]: https://docs.rs/winapi/0.3/x86_64-pc-windows-msvc/winapi/um/d3d11/struct.ID3D11Device.html
    pub fn new(im_ctx: &mut imgui::Context, device: &ID3D11Device) -> Result<Self> {
        unsafe {
            let (vertex_shader, input_layout, constant_buffer) =
                Self::create_vertex_shader(device)?;
            let pixel_shader = Self::create_pixel_shader(device)?;
            let (blend_state, rasterizer_state, depth_stencil_state) =
                Self::create_device_objects(device)?;
            let (font_resource_view, font_sampler) =
                Self::create_font_texture(im_ctx.fonts(), device)?;
            let vertex_buffer = Self::create_vertex_buffer(device, 0)?;
            let index_buffer = Self::create_index_buffer(device, 0)?;

            let context = device.GetImmediateContext();

            im_ctx.io_mut().backend_flags |= BackendFlags::RENDERER_HAS_VTX_OFFSET;
            let renderer_name = concat!("imgui_dx11_renderer@", env!("CARGO_PKG_VERSION"));
            im_ctx.set_renderer_name(Some(renderer_name.to_string()));

            Ok(Renderer {
                device: device.clone(),
                context: context.unwrap(),
                vertex_shader,
                pixel_shader,
                input_layout,
                constant_buffer,
                blend_state,
                rasterizer_state,
                depth_stencil_state,
                font_resource_view,
                font_sampler,
                vertex_buffer,
                index_buffer,
                textures: Textures::new(),
            })
        }
    }

    /// The textures registry of this renderer.
    ///
    /// The texture slot at !0 is reserved for the font texture, therefore the
    /// renderer will ignore any texture inserted into said slot.
    #[inline]
    pub fn textures_mut(&mut self) -> &mut Textures<ID3D11ShaderResourceView> {
        &mut self.textures
    }

    /// The textures registry of this renderer.
    #[inline]
    pub fn textures(&self) -> &Textures<ID3D11ShaderResourceView> {
        &self.textures
    }

    /// Renders the given [`Ui`] with this renderer.
    ///
    /// Should the [`DrawData`] contain an invalid texture index the renderer
    /// will return `DXGI_ERROR_INVALID_CALL` and immediately stop rendering.
    ///
    /// [`Ui`]: https://docs.rs/imgui/*/imgui/struct.Ui.html
    pub fn render(&mut self, draw_data: &DrawData) -> Result<()> {
        if draw_data.display_size[0] <= 0.0 || draw_data.display_size[1] <= 0.0 {
            return Ok(());
        }
        unsafe {
            if self.vertex_buffer.len() < draw_data.total_vtx_count as usize {
                self.vertex_buffer =
                    Self::create_vertex_buffer(&self.device, draw_data.total_vtx_count as usize)?;
            }
            if self.index_buffer.len() < draw_data.total_idx_count as usize {
                self.index_buffer =
                    Self::create_index_buffer(&self.device, draw_data.total_idx_count as usize)?;
            }
            let _state_guard = StateBackup::backup(Some(self.context.clone()));

            self.write_buffers(draw_data)?;
            self.setup_render_state(draw_data);
            self.render_impl(draw_data)?;
        }
        Ok(())
    }

    unsafe fn render_impl(&self, draw_data: &DrawData) -> Result<()> {
        let clip_off = draw_data.display_pos;
        let clip_scale = draw_data.framebuffer_scale;
        let mut vertex_offset = 0;
        let mut index_offset = 0;
        let mut last_tex = TextureId::from(FONT_TEX_ID);
        let context = &self.context;
        context.PSSetShaderResources(0, Some(&[self.font_resource_view.clone()]));
        for draw_list in draw_data.draw_lists() {
            for cmd in draw_list.commands() {
                match cmd {
                    DrawCmd::Elements {
                        count,
                        cmd_params: DrawCmdParams { clip_rect, texture_id, .. },
                    } => {
                        if texture_id != last_tex {
                            let texture = if texture_id.id() == FONT_TEX_ID {
                                self.font_resource_view.clone()
                            } else {
                                self.textures
                                    .get(texture_id)
                                    .ok_or(DXGI_ERROR_INVALID_CALL)?
                                    .clone()
                            };
                            context.PSSetShaderResources(0, Some(&[texture]));
                            last_tex = texture_id;
                        }

                        let r = RECT {
                            left: ((clip_rect[0] - clip_off[0]) * clip_scale[0]) as i32,
                            top: ((clip_rect[1] - clip_off[1]) * clip_scale[1]) as i32,
                            right: ((clip_rect[2] - clip_off[0]) * clip_scale[0]) as i32,
                            bottom: ((clip_rect[3] - clip_off[1]) * clip_scale[1]) as i32,
                        };
                        context.RSSetScissorRects(Some(&[r]));
                        context.DrawIndexed(
                            count as u32,
                            index_offset as u32,
                            vertex_offset as i32,
                        );
                        index_offset += count;
                    },
                    DrawCmd::ResetRenderState => self.setup_render_state(draw_data),
                    DrawCmd::RawCallback { callback, raw_cmd } => {
                        callback(draw_list.raw(), raw_cmd)
                    },
                }
            }
            vertex_offset += draw_list.vtx_buffer().len();
        }
        Ok(())
    }

    unsafe fn setup_render_state(&self, draw_data: &DrawData) {
        let ctx = &self.context;
        let vp = D3D11_VIEWPORT {
            TopLeftX: 0.0,
            TopLeftY: 0.0,
            Width: draw_data.display_size[0] * draw_data.framebuffer_scale[0],
            Height: draw_data.display_size[1] * draw_data.framebuffer_scale[1],
            MinDepth: 0.0,
            MaxDepth: 1.0,
        };
        let draw_fmt = if mem::size_of::<DrawIdx>() == 2 {
            DXGI_FORMAT_R16_UINT
        } else {
            DXGI_FORMAT_R32_UINT
        };
        let stride = mem::size_of::<DrawVert>() as u32;
        let blend_factor = 0.0;

        ctx.RSSetViewports(Some(&[vp]));
        ctx.IASetInputLayout(&self.input_layout);
        ctx.IASetVertexBuffers(
            0,
            1,
            Some(&Some(self.vertex_buffer.get_buf().clone())),
            Some(&stride),
            Some(&0),
        );
        ctx.IASetIndexBuffer(self.index_buffer.get_buf(), draw_fmt, 0);
        ctx.IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
        ctx.VSSetShader(&self.vertex_shader, None);
        ctx.VSSetConstantBuffers(0, Some(&[self.constant_buffer.clone()]));
        ctx.PSSetShader(&self.pixel_shader, None);
        ctx.PSSetSamplers(0, Some(&[self.font_sampler.clone()]));
        ctx.GSSetShader(None, None);
        ctx.HSSetShader(None, None);
        ctx.DSSetShader(None, None);
        ctx.CSSetShader(None, None);
        ctx.OMSetBlendState(&self.blend_state, Some(&blend_factor), 0xFFFFFFFF);
        ctx.OMSetDepthStencilState(&self.depth_stencil_state, 0);
        ctx.RSSetState(&self.rasterizer_state);
    }

    unsafe fn create_vertex_buffer(device: &ID3D11Device, vtx_count: usize) -> Result<Buffer> {
        let len = vtx_count + VERTEX_BUF_ADD_CAPACITY;
        let desc = D3D11_BUFFER_DESC {
            ByteWidth: (len * mem::size_of::<DrawVert>()) as u32,
            Usage: D3D11_USAGE_DYNAMIC,
            BindFlags: D3D11_BIND_VERTEX_BUFFER,
            CPUAccessFlags: D3D11_CPU_ACCESS_WRITE,
            MiscFlags: D3D11_RESOURCE_MISC_FLAG::default(),
            StructureByteStride: 0,
        };

        let mut uninitialized_buffer = None;
        device.CreateBuffer(&desc, None, Some(&mut uninitialized_buffer))?;
        Ok(Buffer(uninitialized_buffer.unwrap(), len))
    }

    unsafe fn create_index_buffer(device: &ID3D11Device, idx_count: usize) -> Result<Buffer> {
        let len = idx_count + INDEX_BUF_ADD_CAPACITY;
        let desc = D3D11_BUFFER_DESC {
            ByteWidth: (len * mem::size_of::<DrawIdx>()) as u32,
            Usage: D3D11_USAGE_DYNAMIC,
            BindFlags: D3D11_BIND_INDEX_BUFFER,
            CPUAccessFlags: D3D11_CPU_ACCESS_WRITE,
            MiscFlags: D3D11_RESOURCE_MISC_FLAG::default(),
            StructureByteStride: 0,
        };

        let mut uninitialized_buffer = None;
        device.CreateBuffer(&desc, None, Some(&mut uninitialized_buffer))?;
        Ok(Buffer(uninitialized_buffer.unwrap(), len))
    }

    unsafe fn write_buffers(&self, draw_data: &DrawData) -> Result<()> {
        let mut vtx_resource = D3D11_MAPPED_SUBRESOURCE::default();
        self.context.Map(
            self.vertex_buffer.get_buf(),
            0,
            D3D11_MAP_WRITE_DISCARD,
            0,
            Some(&mut vtx_resource),
        )?;
        let mut idx_resource = D3D11_MAPPED_SUBRESOURCE::default();
        self.context.Map(
            self.index_buffer.get_buf(),
            0,
            D3D11_MAP_WRITE_DISCARD,
            0,
            Some(&mut idx_resource),
        )?;

        let mut vtx_dst = slice::from_raw_parts_mut(
            vtx_resource.pData.cast::<DrawVert>(),
            draw_data.total_vtx_count as usize,
        );
        let mut idx_dst = slice::from_raw_parts_mut(
            idx_resource.pData.cast::<DrawIdx>(),
            draw_data.total_idx_count as usize,
        );

        for (vbuf, ibuf) in
            draw_data.draw_lists().map(|draw_list| (draw_list.vtx_buffer(), draw_list.idx_buffer()))
        {
            vtx_dst[..vbuf.len()].copy_from_slice(vbuf);
            idx_dst[..ibuf.len()].copy_from_slice(ibuf);
            vtx_dst = &mut vtx_dst[vbuf.len()..];
            idx_dst = &mut idx_dst[ibuf.len()..];
        }

        self.context.Unmap(self.vertex_buffer.get_buf(), 0);
        self.context.Unmap(self.index_buffer.get_buf(), 0);

        let mut mapped_resource = D3D11_MAPPED_SUBRESOURCE::default();
        self.context.Map(
            &self.constant_buffer,
            0,
            D3D11_MAP_WRITE_DISCARD,
            0,
            Some(&mut mapped_resource),
        )?;
        let l = draw_data.display_pos[0];
        let r = draw_data.display_pos[0] + draw_data.display_size[0];
        let t = draw_data.display_pos[1];
        let b = draw_data.display_pos[1] + draw_data.display_size[1];
        let mvp = [
            [2.0 / (r - l), 0.0, 0.0, 0.0],
            [0.0, 2.0 / (t - b), 0.0, 0.0],
            [0.0, 0.0, 0.5, 0.0],
            [(r + l) / (l - r), (t + b) / (b - t), 0.5, 1.0],
        ];
        *mapped_resource.pData.cast::<VertexConstantBuffer>() = VertexConstantBuffer { mvp };
        self.context.Unmap(&self.constant_buffer, 0);

        Ok(())
    }

    unsafe fn create_font_texture(
        mut fonts: &mut imgui::FontAtlas,
        device: &ID3D11Device,
    ) -> Result<(ID3D11ShaderResourceView, ID3D11SamplerState)> {
        let fa_tex = fonts.build_rgba32_texture();

        let desc = D3D11_TEXTURE2D_DESC {
            Width: fa_tex.width,
            Height: fa_tex.height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_R8G8B8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_SHADER_RESOURCE,
            ..Default::default()
        };
        let sub_resource = D3D11_SUBRESOURCE_DATA {
            pSysMem: fa_tex.data.as_ptr().cast(),
            SysMemPitch: desc.Width * 4,
            SysMemSlicePitch: 0,
        };

        let mut uninit_texture = None;
        device.CreateTexture2D(&desc, Some(&sub_resource), Some(&mut uninit_texture))?;
        let texture = uninit_texture.unwrap();

        let mut srv_desc = D3D11_SHADER_RESOURCE_VIEW_DESC {
            Format: DXGI_FORMAT_R8G8B8A8_UNORM,
            ViewDimension: D3D11_SRV_DIMENSION_TEXTURE2D,
            ..Default::default()
        };
        srv_desc.Anonymous.Texture2D.MipLevels = desc.MipLevels;
        srv_desc.Anonymous.Texture2D.MostDetailedMip = 0;
        let mut uninit_font_texture_view = None;
        device.CreateShaderResourceView(
            &texture,
            Some(&srv_desc),
            Some(&mut uninit_font_texture_view),
        )?;
        let font_texture_view = uninit_font_texture_view.unwrap();

        fonts.tex_id = TextureId::from(FONT_TEX_ID);

        let desc = D3D11_SAMPLER_DESC {
            Filter: D3D11_FILTER_MIN_MAG_MIP_LINEAR,
            AddressU: D3D11_TEXTURE_ADDRESS_WRAP,
            AddressV: D3D11_TEXTURE_ADDRESS_WRAP,
            AddressW: D3D11_TEXTURE_ADDRESS_WRAP,
            MipLODBias: 0.0,
            ComparisonFunc: D3D11_COMPARISON_ALWAYS,
            MinLOD: 0.0,
            MaxLOD: 0.0,
            ..Default::default()
        };
        let mut uninit_font_sampler = None;
        device.CreateSamplerState(&desc, Some(&mut uninit_font_sampler))?;
        Ok((font_texture_view, uninit_font_sampler.unwrap()))
    }

    unsafe fn create_vertex_shader(
        device: &ID3D11Device,
    ) -> Result<(ID3D11VertexShader, ID3D11InputLayout, ID3D11Buffer)> {
        const VERTEX_SHADER: &[u8] =
            include_bytes!(concat!(env!("OUT_DIR"), "/vertex_shader.vs_4_0"));
        let mut uninit_vs_shader = None;
        device.CreateVertexShader(VERTEX_SHADER, None, Some(&mut uninit_vs_shader))?;
        let vs_shader = uninit_vs_shader.unwrap();

        let local_layout = [
            D3D11_INPUT_ELEMENT_DESC {
                SemanticName: PCSTR(b"POSITION\0".as_ptr()),
                SemanticIndex: 0,
                Format: DXGI_FORMAT_R32G32_FLOAT,
                InputSlot: 0,
                AlignedByteOffset: D3D11_APPEND_ALIGNED_ELEMENT,
                InputSlotClass: D3D11_INPUT_PER_VERTEX_DATA,
                InstanceDataStepRate: 0,
            },
            D3D11_INPUT_ELEMENT_DESC {
                SemanticName: PCSTR(b"TEXCOORD\0".as_ptr()),
                SemanticIndex: 0,
                Format: DXGI_FORMAT_R32G32_FLOAT,
                InputSlot: 0,
                AlignedByteOffset: D3D11_APPEND_ALIGNED_ELEMENT,
                InputSlotClass: D3D11_INPUT_PER_VERTEX_DATA,
                InstanceDataStepRate: 0,
            },
            D3D11_INPUT_ELEMENT_DESC {
                SemanticName: PCSTR(b"COLOR\0".as_ptr()),
                SemanticIndex: 0,
                Format: DXGI_FORMAT_R8G8B8A8_UNORM,
                InputSlot: 0,
                AlignedByteOffset: D3D11_APPEND_ALIGNED_ELEMENT,
                InputSlotClass: D3D11_INPUT_PER_VERTEX_DATA,
                InstanceDataStepRate: 0,
            },
        ];

        let mut uninit_input_layout = None;
        device.CreateInputLayout(&local_layout, VERTEX_SHADER, Some(&mut uninit_input_layout))?;
        let input_layout = uninit_input_layout.unwrap();

        let desc = D3D11_BUFFER_DESC {
            ByteWidth: mem::size_of::<VertexConstantBuffer>() as _,
            Usage: D3D11_USAGE_DYNAMIC,
            BindFlags: D3D11_BIND_CONSTANT_BUFFER,
            CPUAccessFlags: D3D11_CPU_ACCESS_WRITE,
            MiscFlags: D3D11_RESOURCE_MISC_FLAG::default(),
            StructureByteStride: 0,
        };
        let mut uninit_vertex_constant_buffer = None;
        device.CreateBuffer(&desc, None, Some(&mut uninit_vertex_constant_buffer))?;
        Ok((vs_shader, input_layout, uninit_vertex_constant_buffer.unwrap()))
    }

    unsafe fn create_pixel_shader(device: &ID3D11Device) -> Result<ID3D11PixelShader> {
        const PIXEL_SHADER: &[u8] =
            include_bytes!(concat!(env!("OUT_DIR"), "/pixel_shader.ps_4_0"));
        let mut uninit_pixel_shader = None;
        device.CreatePixelShader(PIXEL_SHADER, None, Some(&mut uninit_pixel_shader))?;
        Ok(uninit_pixel_shader.unwrap())
    }

    unsafe fn create_device_objects(
        device: &ID3D11Device,
    ) -> Result<(ID3D11BlendState, ID3D11RasterizerState, ID3D11DepthStencilState)> {
        let desc = D3D11_BLEND_DESC {
            AlphaToCoverageEnable: false.into(),
            IndependentBlendEnable: true.into(),
            RenderTarget: [D3D11_RENDER_TARGET_BLEND_DESC {
                BlendEnable: true.into(),
                SrcBlend: D3D11_BLEND_SRC_ALPHA,
                DestBlend: D3D11_BLEND_INV_SRC_ALPHA,
                BlendOp: D3D11_BLEND_OP_ADD,
                SrcBlendAlpha: D3D11_BLEND_ONE,
                DestBlendAlpha: D3D11_BLEND_INV_SRC_ALPHA,
                BlendOpAlpha: D3D11_BLEND_OP_ADD,
                RenderTargetWriteMask: D3D11_COLOR_WRITE_ENABLE_ALL.0 as u8,
            }; 8],
        };

        let mut uninit_blend_state = None;
        device.CreateBlendState(&desc, Some(&mut uninit_blend_state))?;

        let desc = D3D11_RASTERIZER_DESC {
            FillMode: D3D11_FILL_SOLID,
            CullMode: D3D11_CULL_NONE,
            DepthClipEnable: true.into(),
            ScissorEnable: true.into(),
            ..Default::default()
        };
        let mut uninit_rasterizer_state = None;
        device.CreateRasterizerState(&desc, Some(&mut uninit_rasterizer_state))?;

        let stencil_op_desc = D3D11_DEPTH_STENCILOP_DESC {
            StencilFailOp: D3D11_STENCIL_OP_KEEP,
            StencilDepthFailOp: D3D11_STENCIL_OP_KEEP,
            StencilPassOp: D3D11_STENCIL_OP_KEEP,
            StencilFunc: D3D11_COMPARISON_ALWAYS,
        };
        let desc = D3D11_DEPTH_STENCIL_DESC {
            DepthEnable: false.into(),
            DepthWriteMask: D3D11_DEPTH_WRITE_MASK_ALL,
            DepthFunc: D3D11_COMPARISON_ALWAYS,
            StencilEnable: false.into(),
            StencilReadMask: 0,
            StencilWriteMask: 0,
            FrontFace: stencil_op_desc,
            BackFace: stencil_op_desc,
        };
        let mut uninit_depth_stencil_state = None;
        device.CreateDepthStencilState(&desc, Some(&mut uninit_depth_stencil_state))?;
        Ok((
            uninit_blend_state.unwrap(),
            uninit_rasterizer_state.unwrap(),
            uninit_depth_stencil_state.unwrap(),
        ))
    }
}

#[derive(Debug)]
struct Buffer(ID3D11Buffer, usize);

impl Buffer {
    #[inline]
    fn len(&self) -> usize {
        self.1
    }
    #[inline]
    fn get_buf(&self) -> &ID3D11Buffer {
        &self.0
    }
}

#[derive(Debug, Default)]
struct StateBackup {
    context: Option<ID3D11DeviceContext>,
    scissor_rects: RECT,
    viewports: D3D11_VIEWPORT,
    rasterizer_state: Option<ID3D11RasterizerState>,
    blend_state: Option<ID3D11BlendState>,
    blend_factor: f32,
    sample_mask: u32,
    depth_stencil_state: Option<ID3D11DepthStencilState>,
    stencil_ref: u32,
    shader_resource: Vec<Option<ID3D11ShaderResourceView>>,
    sampler: Vec<Option<ID3D11SamplerState>>,
    ps_shader: Option<ID3D11PixelShader>,
    ps_instances: Option<ID3D11ClassInstance>,
    vs_shader: Option<ID3D11VertexShader>,
    vs_instances: Option<ID3D11ClassInstance>,
    constant_buffer: Vec<Option<ID3D11Buffer>>,
    gs_shader: Option<ID3D11GeometryShader>,
    gs_instances: Option<ID3D11ClassInstance>,
    index_buffer: Option<ID3D11Buffer>,
    index_buffer_offset: u32,
    index_buffer_format: DXGI_FORMAT,
    vertex_buffer: Option<ID3D11Buffer>,
    vertex_buffer_offset: u32,
    vertex_buffer_stride: u32,
    topology: D3D_PRIMITIVE_TOPOLOGY,
    input_layout: Option<ID3D11InputLayout>,
}

impl StateBackup {
    unsafe fn backup(context: Option<ID3D11DeviceContext>) -> Self {
        let mut result = Self::default();
        let ctx = context.as_ref().unwrap();
        result.topology = ctx.IAGetPrimitiveTopology();
        ctx.IAGetIndexBuffer(
            Some(&mut result.index_buffer),
            Some(&mut result.index_buffer_format),
            Some(&mut result.index_buffer_offset),
        );
        ctx.IAGetVertexBuffers(
            0,
            1,
            Some(&mut result.vertex_buffer),
            Some(&mut result.vertex_buffer_stride),
            Some(&mut result.vertex_buffer_offset),
        );
        result.input_layout = ctx.IAGetInputLayout().ok();
        ctx.VSGetShader(&mut result.vs_shader, Some(&mut result.vs_instances), Some(&mut 256));
        ctx.VSGetConstantBuffers(0, Some(&mut result.constant_buffer));
        ctx.GSGetShader(&mut result.gs_shader, Some(&mut result.gs_instances), Some(&mut 256));
        ctx.RSGetViewports(&mut 1, Some(&mut result.viewports));
        ctx.RSGetScissorRects(&mut 1, Some(&mut result.scissor_rects));
        result.rasterizer_state = ctx.RSGetState().ok();
        ctx.PSGetShaderResources(0, Some(&mut result.shader_resource));
        ctx.PSGetSamplers(0, Some(&mut result.sampler));
        ctx.PSGetShader(&mut result.ps_shader, Some(&mut result.ps_instances), Some(&mut 256));
        ctx.OMGetBlendState(
            Some(&mut result.blend_state),
            Some(&mut result.blend_factor),
            Some(&mut result.sample_mask),
        );
        ctx.OMGetDepthStencilState(
            Some(&mut result.depth_stencil_state),
            Some(&mut result.stencil_ref),
        );
        result.context = context;
        result
    }

    fn filter_none<T: Clone>(option_vec: &Vec<Option<T>>) -> Vec<T> {
        option_vec.iter().filter_map(|x| x.as_ref().cloned()).collect::<Vec<_>>()
    }

    pub fn restore(&mut self) {
        unsafe {
            if self.context.is_none() { return };

            let ctx = self.context.as_ref().unwrap();

            ctx.RSSetScissorRects(Some(&[self.scissor_rects]));
            ctx.RSSetViewports(Some(&[self.viewports]));
            ctx.RSSetState(self.rasterizer_state.as_ref());
            ctx.OMSetBlendState(self.blend_state.as_ref(), Some(&self.blend_factor), 0xFFFFFFFF);
            ctx.OMSetDepthStencilState(self.depth_stencil_state.as_ref(), self.stencil_ref);
            ctx.PSSetShaderResources(0, Some(&Self::filter_none(&self.shader_resource)));
            ctx.PSSetSamplers(0, Some(&Self::filter_none(&self.sampler)));
            ctx.PSSetShader(
                self.ps_shader.as_ref(),
                self.ps_instances.as_ref().map(slice::from_ref),
            );
            ctx.VSSetShader(
                self.vs_shader.as_ref(),
                self.vs_instances.as_ref().map(slice::from_ref),
            );
            ctx.VSSetConstantBuffers(0, Some(&Self::filter_none(&self.constant_buffer)));
            ctx.GSSetShader(
                self.gs_shader.as_ref(),
                self.gs_instances.as_ref().map(slice::from_ref),
            );
            ctx.IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            ctx.IASetIndexBuffer(
                self.index_buffer.as_ref(),
                self.index_buffer_format,
                self.index_buffer_offset,
            );
            ctx.IASetVertexBuffers(
                0,
                1,
                Some(&self.vertex_buffer),
                Some(&self.vertex_buffer_stride),
                Some(&self.vertex_buffer_offset),
            );
            ctx.IASetInputLayout(self.input_layout.as_ref());
        }
    }
}

impl Drop for StateBackup {
    fn drop(&mut self) {
        self.restore();
    }
}
