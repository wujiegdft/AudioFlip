#![allow(non_snake_case)]

use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::StructuredStorage::PropVariantToStringAlloc;
use windows::Win32::System::Com::*;
use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;
use windows::core::*;

// Undocumented but stable since Windows 7; it is the only way to change the default endpoint.
// Only SetDefaultEndpoint is called, the other slots just keep the vtable layout right.
#[windows_core::interface("f8679f50-850a-41cf-9c72-430f290290c8")]
unsafe trait IPolicyConfig: IUnknown {
    fn GetMixFormat(&self) -> HRESULT;
    fn GetDeviceFormat(&self) -> HRESULT;
    fn ResetDeviceFormat(&self) -> HRESULT;
    fn SetDeviceFormat(&self) -> HRESULT;
    fn GetProcessingPeriod(&self) -> HRESULT;
    fn SetProcessingPeriod(&self) -> HRESULT;
    fn GetShareMode(&self) -> HRESULT;
    fn SetShareMode(&self) -> HRESULT;
    fn GetPropertyValue(&self) -> HRESULT;
    fn SetPropertyValue(&self) -> HRESULT;
    fn SetDefaultEndpoint(&self, device_id: PCWSTR, role: ERole) -> HRESULT;
    fn SetEndpointVisibility(&self) -> HRESULT;
}

const CLSID_POLICY_CONFIG_CLIENT: GUID = GUID::from_u128(0x870af99c_171d_4f9e_af0d_e63df40c2bc9);

const PKEY_DEVICECLASS_ICONPATH: PROPERTYKEY = PROPERTYKEY {
    fmtid: GUID::from_u128(0x259abffc_50a7_47ce_af08_68c9a7d73366),
    pid: 12,
};

#[derive(Clone, Debug)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub icon_path: String,
}

pub struct Audio {
    enumerator: IMMDeviceEnumerator,
    policy: IPolicyConfig,
}

impl Audio {
    pub fn new() -> Result<Self> {
        unsafe {
            Ok(Self {
                enumerator: CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?,
                policy: CoCreateInstance(&CLSID_POLICY_CONFIG_CLIENT, None, CLSCTX_ALL)?,
            })
        }
    }

    pub fn devices(&self) -> Vec<Device> {
        let mut out = Vec::new();
        unsafe {
            let Ok(coll) = self.enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE) else {
                return out;
            };
            let count = coll.GetCount().unwrap_or(0);
            for i in 0..count {
                if let Ok(dev) = coll.Item(i) {
                    if let Some(d) = read_device(&dev) {
                        out.push(d);
                    }
                }
            }
        }
        out
    }

    pub fn default_id(&self) -> Option<String> {
        unsafe {
            let dev = self.enumerator.GetDefaultAudioEndpoint(eRender, eConsole).ok()?;
            take_pwstr(dev.GetId().ok()?)
        }
    }

    pub fn set_default(&self, id: &str) -> Result<()> {
        let wide: Vec<u16> = id.encode_utf16().chain(Some(0)).collect();
        let pid = PCWSTR(wide.as_ptr());
        unsafe {
            for role in [eConsole, eMultimedia, eCommunications] {
                self.policy.SetDefaultEndpoint(pid, role).ok()?;
            }
        }
        Ok(())
    }

    pub fn register(&self, client: &IMMNotificationClient) -> Result<()> {
        unsafe { self.enumerator.RegisterEndpointNotificationCallback(client) }
    }

    pub fn unregister(&self, client: &IMMNotificationClient) {
        unsafe {
            let _ = self.enumerator.UnregisterEndpointNotificationCallback(client);
        }
    }
}

unsafe fn take_pwstr(p: PWSTR) -> Option<String> {
    if p.is_null() {
        return None;
    }
    let s = unsafe { p.to_string().ok() };
    unsafe { CoTaskMemFree(Some(p.0 as *const _)) };
    s
}

unsafe fn prop_string(store: &IPropertyStore, key: &PROPERTYKEY) -> Option<String> {
    unsafe {
        let pv = store.GetValue(key).ok()?;
        take_pwstr(PropVariantToStringAlloc(&pv).ok()?).filter(|s| !s.is_empty())
    }
}

unsafe fn read_device(dev: &IMMDevice) -> Option<Device> {
    unsafe {
        let id = take_pwstr(dev.GetId().ok()?)?;
        let store = dev.OpenPropertyStore(STGM_READ).ok()?;
        let name = prop_string(&store, &PKEY_Device_FriendlyName).unwrap_or_else(|| id.clone());
        let icon_path = prop_string(&store, &PKEY_DEVICECLASS_ICONPATH)
            .unwrap_or_else(|| r"%SystemRoot%\System32\mmres.dll,-3010".into());
        Some(Device { id, name, icon_path })
    }
}

#[windows_core::implement(IMMNotificationClient)]
pub struct Notifier {
    hwnd: isize,
    msg: u32,
}

impl Notifier {
    pub fn create(hwnd: isize, msg: u32) -> IMMNotificationClient {
        Notifier { hwnd, msg }.into()
    }
}

impl Notifier {
    fn ping(&self) -> Result<()> {
        use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
        use windows::Win32::UI::WindowsAndMessaging::PostMessageW;
        unsafe {
            let _ = PostMessageW(Some(HWND(self.hwnd as _)), self.msg, WPARAM(0), LPARAM(0));
        }
        Ok(())
    }
}

impl IMMNotificationClient_Impl for Notifier_Impl {
    fn OnDeviceStateChanged(&self, _: &PCWSTR, _: DEVICE_STATE) -> Result<()> {
        self.ping()
    }
    fn OnDeviceAdded(&self, _: &PCWSTR) -> Result<()> {
        self.ping()
    }
    fn OnDeviceRemoved(&self, _: &PCWSTR) -> Result<()> {
        self.ping()
    }
    fn OnDefaultDeviceChanged(&self, flow: EDataFlow, role: ERole, _: &PCWSTR) -> Result<()> {
        if flow == eRender && role == eConsole {
            self.ping()?;
        }
        Ok(())
    }
    fn OnPropertyValueChanged(&self, _: &PCWSTR, _: &PROPERTYKEY) -> Result<()> {
        Ok(())
    }
}
