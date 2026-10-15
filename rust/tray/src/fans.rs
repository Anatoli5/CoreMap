// Обороты вентиляторов ноутбука — для журнала автомодуля: при каких оборотах блок опущен или поднят (DECISIONS #32,
// «Fans»; владелец 15.10: «нужно запоминать… какая при этом скорость вентилятора…»).
//
// Доступ — служебный интерфейс прошивки HP в WMI, тот же, что у HpFans в src/Fans.cs (что о нём известно и откуда —
// outputs\fan-interface-2026-10-13.md): пространство root\wmi, экземпляр класса hpqBIntM, метод hpqBIOSInt128 (ответ
// 128 байт), на входе объект hpqBDataIn: подпись «SECU», команда 0x20008, запрос 0x2D, четыре нулевых байта данных.
// В ответе байт 0 — вентилятор процессора, байт 1 — видеокарты, единица 100 об/мин.
//
// Программа входа прошивку только читает. Запрос 0x2D собирается один раз при подключении, и другого этот модуль
// послать не может: функции «вызвать прошивку с таким-то запросом» в нём нет (так же pawnio.rs пишет только один
// регистр). «Максимум» и ручной уровень вентиляторов, которые умеет C#-часть, сюда не перенесены.
//
// Почему крейт windows, а не windows-sys, как в остальной программе: в windows-sys нет COM-интерфейсов (IWbemServices
// и прочих), а писать их таблицы методов самим — больше unsafe и больше мест для ошибки. Крейт уже был в Cargo.lock
// (его тянет Slint), новой загрузки нет.
//
// Поток. COM — однопоточное подразделение (STA) того потока, который открыл HpFans; у программы входа это поток окна
// с циклом сообщений, а STA требует именно его. HpFans с этого потока не уходит (не Send): COM-объекты подразделения
// и CoUninitialize принадлежат ему. Пока идёт вызов, COM крутит сообщения окна — процедура окна только складывает
// события в очередь (shell.rs), так что повторного входа в логику нет.
//
// Вызов прошивки может зависнуть (src/Fans.cs: поэтому там отдельный поток), а здесь он идёт в потоке значка и
// автомодуля. Поэтому вызов полусинхронный: ответ ждётся не дольше PATIENCE_MS; не пришёл — ошибка, а пока прошлый
// вызов не кончился, новый не посылается и опрос сразу возвращает ошибку, не дожидаясь снова.
#![expect(
    unsafe_code,
    reason = "WMI is reached through COM: CoInitializeEx, interface methods, VARIANT and SAFEARRAY are FFI"
)]

use std::ffi::c_void;
use std::marker::PhantomData;
use std::mem::ManuallyDrop;
use std::ptr::null_mut;

use machine::HwError;
use windows::Win32::Foundation::{E_NOINTERFACE, RPC_E_CHANGED_MODE};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoCreateInstance, CoInitializeEx,
    CoSetProxyBlanket, CoUninitialize, EOAC_NONE, RPC_C_AUTHN_LEVEL_CALL, RPC_C_IMP_LEVEL_IMPERSONATE,
};
use windows::Win32::System::Ole::{
    SafeArrayAccessData, SafeArrayCreateVector, SafeArrayGetDim, SafeArrayGetLBound, SafeArrayGetUBound,
    SafeArrayUnaccessData,
};
use windows::Win32::System::Variant::{
    VARENUM, VARIANT, VARIANT_0, VARIANT_0_0, VARIANT_0_0_0, VT_ARRAY, VT_BSTR, VT_I4, VT_UI1, VT_UI4, VT_UNKNOWN,
    VariantClear,
};
use windows::Win32::System::Wmi::{
    IWbemCallResult, IWbemClassObject, IWbemLocator, IWbemServices, WBEM_E_INVALID_CLASS, WBEM_E_INVALID_NAMESPACE,
    WBEM_FLAG_FORWARD_ONLY, WBEM_FLAG_RETURN_IMMEDIATELY, WBEM_FLAG_RETURN_WBEM_COMPLETE, WBEM_S_TIMEDOUT, WbemLocator,
};
use windows::core::{BSTR, IUnknown, Interface, PCWSTR};

/// Обороты вентиляторов: настоящие (`HpFans`) или имитация (`SimFans`).
pub trait Fans: std::fmt::Debug {
    /// Обороты вентиляторов, об/мин: процессора, затем видеокарты. Ошибка — текст для журнала.
    fn rpm(&mut self) -> Result<Vec<u32>, machine::HwError>;
}

/// Единица оборотов в ответе прошивки, об/мин.
const UNIT: u32 = 100;
/// Больше этого у вентилятора ноутбука не бывает: такое число — ошибка, а не обороты. 14.10 прошивка трижды
/// ответила 12 800–16 700 (KNOWLEDGE.md, вентиляторы); причина неизвестна.
const PLAUSIBLE: u32 = 9_000;

/// Пространство WMI, класс с методами прошивки и класс входных данных.
const NAMESPACE: &str = r"ROOT\WMI";
const CLASS: &str = "hpqBIntM";
const INPUT: &str = "hpqBDataIn";
/// Метод с ответом в 128 байт: на запрос 0x2D прошивка отвечает столько.
const METHOD: &str = "hpqBIOSInt128";
/// Подпись, команда и запрос «обороты» (src/Fans.cs: `Sign`, `Command`, `Levels`).
const SIGN: [u8; 4] = *b"SECU";
const COMMAND: i32 = 0x2_0008;
const READ: i32 = 0x2D;
/// Данные запроса: четыре нулевых байта, как у C#-части и обеих программ под Windows из источников.
const DATA: [u8; 4] = [0; 4];
/// Сколько ждать ответа прошивки и списка экземпляров, мс: столько же ждёт C#-часть (`HpFans.Patience`).
const PATIENCE_MS: i32 = 4_000;
/// Проверка подлинности для `CoSetProxyBlanket` (rpcdce.h; в крейте — `Win32::System::Rpc`, ради двух чисел этот
/// модуль не подключается): Windows NTLM/Kerberos по выбору системы и без отдельной авторизации.
const RPC_C_AUTHN_WINNT: u32 = 10;
const RPC_C_AUTHZ_NONE: u32 = 0;

/// Текст, когда интерфейса прошивки на машине нет (как в src/Fans.cs).
const ABSENT: &str = "служебного интерфейса прошивки HP на этой машине нет";

/// Ответ прошивки на запрос 0x2D — в обороты: байт 0 — процессор, байт 1 — видеокарта; остальное не читается.
fn decode(answer: &[u8]) -> Result<Vec<u32>, HwError> {
    let [cpu, gpu, ..] = answer else {
        return Err(HwError(format!(
            "прошивка вернула обороты не того размера: {} байт",
            answer.len()
        )));
    };
    let rpm = vec![u32::from(*cpu) * UNIT, u32::from(*gpu) * UNIT];
    plausible(&rpm)?;
    Ok(rpm)
}

/// Обороты выше `PLAUSIBLE` хотя бы у одного вентилятора — ошибка для всего показания: прошивка ответила не оборотами,
/// и второму числу того же ответа верить тоже нельзя.
fn plausible(rpm: &[u32]) -> Result<(), HwError> {
    if rpm.iter().all(|&v| v <= PLAUSIBLE) {
        return Ok(());
    }
    let list: Vec<String> = rpm.iter().map(u32::to_string).collect();
    Err(HwError(format!(
        "невозможное значение оборотов: {} об/мин (больше {PLAUSIBLE} не бывает)",
        list.join(" / ")
    )))
}

/// Имитация для скрытой проверки: отдаёт заданные обороты или отказ; к WMI не обращается. Правило правдоподобия —
/// то же, что у настоящих: проверка видит «невозможное значение» так же, как его сообщит прошивка.
#[derive(Debug, Clone, Default)]
pub struct SimFans {
    pub rpm: Vec<u32>,
    pub fail: bool,
}

impl Fans for SimFans {
    fn rpm(&mut self) -> Result<Vec<u32>, HwError> {
        if self.fail {
            return Err(HwError("обороты не прочитаны (проверка)".into()));
        }
        plausible(&self.rpm)?;
        // Копия: показание уходит вызывающему, а имитация продолжает отдавать то же; раз в опрос, не чаще 2 с.
        Ok(self.rpm.clone())
    }
}

/// Обороты из прошивки HP. Подключается один раз и держит связь: каждый опрос — один вызов метода прошивки.
/// Связь потерялась (служба WMI перезапущена, вызов отказал) — следующий опрос подключается заново.
#[derive(Debug)]
pub struct HpFans {
    /// None — связи нет, `rpm` подключится заново.
    link: Option<Link>,
    /// Последним полем: поля освобождаются в порядке объявления, и COM на потоке закрывается после всех своих
    /// объектов.
    #[expect(
        dead_code,
        reason = "held only for its Drop: CoUninitialize after every COM object of the thread"
    )]
    com: Apartment,
}

impl HpFans {
    /// Подключается к прошивке на вызывающем потоке; к прошивке ничего не посылает. Ошибка — интерфейса нет
    /// (не HP или нет прав) или WMI не ответила.
    pub fn open() -> Result<Self, HwError> {
        let com = Apartment::enter()?;
        let link = Link::connect()?;
        Ok(Self { link: Some(link), com })
    }
}

impl Fans for HpFans {
    fn rpm(&mut self) -> Result<Vec<u32>, HwError> {
        let mut link = match self.link.take() {
            Some(link) => link,
            None => Link::connect()?,
        };
        let (keep, result) = match link.read() {
            Ok(rpm) => (true, Ok(rpm)),
            Err(Miss::Kept(e)) => (true, Err(e)),
            Err(Miss::Lost(e)) => (false, Err(e)),
        };
        if keep {
            self.link = Some(link);
        }
        result
    }
}

/// COM на этом потоке. `owned` — `CoInitializeEx` здесь удался и при уходе уравновешивается `CoUninitialize`.
#[derive(Debug)]
struct Apartment {
    owned: bool,
    /// Не Send: `CoUninitialize` должна идти на том же потоке, что и `CoInitializeEx`.
    _thread: PhantomData<*const ()>,
}

impl Apartment {
    fn enter() -> Result<Self, HwError> {
        // SAFETY: the reserved argument is null as required; no other pointers are passed.
        let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) };
        if hr == RPC_E_CHANGED_MODE {
            // Поток уже в многопоточном подразделении: COM работает и там, а выходить из него — не нам.
            return Ok(Self {
                owned: false,
                _thread: PhantomData,
            });
        }
        if hr.is_err() {
            return Err(HwError(format!(
                "COM на потоке программы не запустился: {}",
                why(&hr.into())
            )));
        }
        // S_OK — подразделение открыто здесь, S_FALSE — уже было; оба считаются и уравновешиваются одним выходом.
        Ok(Self {
            owned: true,
            _thread: PhantomData,
        })
    }
}

impl Drop for Apartment {
    fn drop(&mut self) {
        if self.owned {
            // SAFETY: balances the successful CoInitializeEx of `enter` on this same thread (the type is not Send);
            // HpFans declares this field last, so every COM object of the apartment is released before.
            unsafe { CoUninitialize() };
        }
    }
}

/// Связь со службой WMI и собранный запрос 0x2D.
#[derive(Debug)]
struct Link {
    services: IWbemServices,
    /// Путь экземпляра `hpqBIntM` (`__RELPATH`), у которого вызывается метод.
    path: BSTR,
    method: BSTR,
    /// Входные параметры метода с запросом 0x2D — единственный запрос, который здесь есть.
    read: IWbemClassObject,
    /// Вызов, на который прошивка не ответила за `PATIENCE_MS`; пока он не кончился, новый не посылается.
    pending: Option<IWbemCallResult>,
}

/// Чем кончился опрос, который не дал оборотов.
enum Miss {
    /// Связь цела (прошивка ответила отказом или чепухой, или ещё не ответила).
    Kept(HwError),
    /// Вызов через WMI не прошёл: связь бросается, следующий опрос подключается заново.
    Lost(HwError),
}

/// Ожидание ответа на полусинхронный вызов.
enum Wait {
    Pending,
    Done(IWbemClassObject),
    Failed(windows::core::Error),
}

impl Link {
    fn connect() -> Result<Self, HwError> {
        // SAFETY: CLSID_WbemLocator, no aggregation, in-process server (wbemprox.dll); COM is initialized on this
        // thread (Apartment::enter runs first in HpFans::open, and HpFans never leaves the thread).
        let locator: IWbemLocator = unsafe { CoCreateInstance(&WbemLocator, None, CLSCTX_INPROC_SERVER) }
            .map_err(|e| HwError(format!("служба WMI не открылась: {}", why(&e))))?;
        let empty = BSTR::new();
        // SAFETY: every string is a live BSTR for the duration of the call (empty = current user, default locale and
        // authority); no context object.
        let services =
            unsafe { locator.ConnectServer(&BSTR::from(NAMESPACE), &empty, &empty, &empty, 0, &empty, None) }
                .map_err(|e| absent_or(&e, r"пространство WMI root\wmi не открылось"))?;
        secure((&services).into()).map_err(|e| HwError(format!("права вызова WMI не заданы: {}", why(&e))))?;
        let path = instance(&services)?;
        let method = BSTR::from(METHOD);
        let read = request(&services, &method)?;
        Ok(Self {
            services,
            path,
            method,
            read,
            pending: None,
        })
    }

    /// Один опрос: вызов метода прошивки с запросом 0x2D и разбор ответа.
    fn read(&mut self) -> Result<Vec<u32>, Miss> {
        // Прошлый вызов кончился — его запоздавший ответ (или ошибка) отбрасывается: показание нужно на сейчас, а не на
        // тот опрос. Не кончился — новый не посылается.
        if let Some(late) = self.pending.take()
            && matches!(result(&late, 0), Wait::Pending)
        {
            self.pending = Some(late);
            return Err(Miss::Kept(HwError(format!(
                "прошивка ещё не ответила на прошлый запрос 0x{READ:X}"
            ))));
        }
        let lost = |e: &windows::core::Error| {
            Miss::Lost(HwError(format!("запрос 0x{READ:X} к прошивке не выполнен: {}", why(e))))
        };
        let mut call = None;
        // SAFETY: path and method are live BSTRs and `read` a live in-parameters object for the call's duration;
        // semisynchronous call: no out-parameters pointer, the call object goes to `call`, which is writable.
        unsafe {
            self.services.ExecMethod(
                &self.path,
                &self.method,
                WBEM_FLAG_RETURN_IMMEDIATELY,
                None,
                Some(&self.read),
                None,
                Some(&raw mut call),
            )
        }
        .map_err(|e| lost(&e))?;
        let Some(call) = call else {
            return Err(lost(&windows::core::Error::empty()));
        };
        secure((&call).into()).map_err(|e| lost(&e))?;
        match result(&call, PATIENCE_MS) {
            Wait::Pending => {
                self.pending = Some(call);
                Err(Miss::Kept(HwError(format!(
                    "прошивка не ответила на запрос 0x{READ:X} за {} с",
                    PATIENCE_MS / 1000
                ))))
            }
            Wait::Failed(e) => Err(lost(&e)),
            Wait::Done(out) => answer(&out).map_err(Miss::Kept),
        }
    }
}

/// Путь первого экземпляра `hpqBIntM` (как в C#: первый из найденных).
fn instance(services: &IWbemServices) -> Result<BSTR, HwError> {
    // SAFETY: the class name is a live BSTR for the call; forward-only enumerator that returns at once; no context.
    let found = unsafe {
        services.CreateInstanceEnum(
            &BSTR::from(CLASS),
            WBEM_FLAG_FORWARD_ONLY | WBEM_FLAG_RETURN_IMMEDIATELY,
            None,
        )
    }
    .map_err(|e| absent_or(&e, "служебный интерфейс прошивки HP не открылся"))?;
    secure((&found).into()).map_err(|e| HwError(format!("права вызова WMI не заданы: {}", why(&e))))?;
    let mut slot = [None];
    let mut got = 0u32;
    // SAFETY: one slot to fill and a writable counter, both live for the call; waits at most PATIENCE_MS.
    let hr = unsafe { found.Next(PATIENCE_MS, &mut slot, &raw mut got) };
    if hr.0 == WBEM_S_TIMEDOUT.0 {
        return Err(HwError(format!("служба WMI не ответила за {} с", PATIENCE_MS / 1000)));
    }
    hr.ok()
        .map_err(|e| absent_or(&e, "служебный интерфейс прошивки HP не открылся"))?;
    let [Some(object)] = slot else {
        return Err(HwError(ABSENT.into()));
    };
    get(&object, "__RELPATH")?
        .text()
        .ok_or_else(|| HwError(format!("у объекта {CLASS} нет пути")))
}

/// Входные параметры метода с запросом 0x2D, собранные один раз: подпись, команда, запрос, размер и данные.
fn request(services: &IWbemServices, method: &BSTR) -> Result<IWbemClassObject, HwError> {
    let fail = |what: &str, e: &windows::core::Error| {
        HwError(format!("запрос оборотов к прошивке не собран ({what}): {}", why(e)))
    };
    let methods = class(services, CLASS).map_err(|e| fail(CLASS, &e))?;
    let mut signature = None;
    let mut output = None;
    // SAFETY: the name is a live BSTR; both out pointers are writable and receive owned references (or None).
    unsafe { methods.GetMethod(method, 0, &raw mut signature, &raw mut output) }.map_err(|e| fail(METHOD, &e))?;
    let Some(signature) = signature else {
        return Err(HwError(format!("у метода {METHOD} нет входных параметров")));
    };
    // SAFETY: plain call on a live class object; the flags argument must be 0.
    let params = unsafe { signature.SpawnInstance(0) }.map_err(|e| fail(METHOD, &e))?;
    let shape = class(services, INPUT).map_err(|e| fail(INPUT, &e))?;
    // SAFETY: as above.
    let data = unsafe { shape.SpawnInstance(0) }.map_err(|e| fail(INPUT, &e))?;
    let size = i32::try_from(DATA.len()).map_err(|e| HwError(e.to_string()))?;
    let set = |object: &IWbemClassObject, name: &str, value: &Var| put(object, name, value).map_err(|e| fail(name, &e));
    set(&data, "Sign", &Var::bytes(&SIGN)?)?;
    set(&data, "Command", &Var::int(COMMAND))?;
    set(&data, "CommandType", &Var::int(READ))?;
    set(&data, "Size", &Var::int(size))?;
    set(&data, "hpqBData", &Var::bytes(&DATA)?)?;
    set(&params, "InData", &Var::object(&data))?;
    Ok(params)
}

/// Разбор ответа: `OutData.rwReturnCode` — 0 при успехе, `OutData.Data` — 128 байт.
fn answer(out: &IWbemClassObject) -> Result<Vec<u32>, HwError> {
    let Some(data) = get(out, "OutData")?.object_of() else {
        return Err(HwError(format!("прошивка на запрос 0x{READ:X} не ответила")));
    };
    let Some(code) = get(&data, "rwReturnCode")?.uint() else {
        return Err(HwError(format!("прошивка на запрос 0x{READ:X} не вернула код ответа")));
    };
    if code != 0 {
        return Err(HwError(format!("прошивка ответила на запрос 0x{READ:X} кодом {code}")));
    }
    let Some(bytes) = get(&data, "Data")?.bytes_of() else {
        return Err(HwError(format!("прошивка на запрос 0x{READ:X} вернула пустой ответ")));
    };
    decode(&bytes)
}

/// Определение класса WMI по имени.
fn class(services: &IWbemServices, name: &str) -> windows::core::Result<IWbemClassObject> {
    let mut object = None;
    // SAFETY: the name is a live BSTR; synchronous call; the object pointer is writable, no call-result pointer.
    unsafe {
        services.GetObject(
            &BSTR::from(name),
            WBEM_FLAG_RETURN_WBEM_COMPLETE,
            None,
            Some(&raw mut object),
            None,
        )
    }?;
    object.ok_or_else(windows::core::Error::empty)
}

/// Ждёт ответа на полусинхронный вызов не дольше `timeout` мс (0 — только посмотреть).
fn result(call: &IWbemCallResult, timeout: i32) -> Wait {
    let mut out: *mut c_void = null_mut();
    // The wrapper `GetResultObject` cannot tell "not yet" (WBEM_S_TIMEDOUT is a success code with no object) from a
    // failure, so the method is called through the interface table to see the status itself.
    // SAFETY: GetResultObject(this, lTimeout, IWbemClassObject**) of a live call object; `out` is writable and on
    // success receives one reference that the caller owns.
    let hr = unsafe { (Interface::vtable(call).GetResultObject)(Interface::as_raw(call), timeout, &raw mut out) };
    if hr.0 == WBEM_S_TIMEDOUT.0 {
        return Wait::Pending;
    }
    if hr.is_err() {
        return Wait::Failed(hr.into());
    }
    if out.is_null() {
        return Wait::Failed(windows::core::Error::empty());
    }
    // SAFETY: a successful GetResultObject handed over one reference to a non-null IWbemClassObject; from_raw
    // takes it over and releases it on drop.
    Wait::Done(unsafe { IWbemClassObject::from_raw(out) })
}

/// Олицетворение на прокси WMI: поставщик прошивки исполняет вызов от имени программы, а уровня «опознать», который
/// COM ставит процессу без `CoInitializeSecurity`, ему мало. Ставится на каждый прокси (службу, перечисление,
/// вызов), а не на весь процесс. Объект без прокси (внутри процесса) настройки не принимает — ему она и не нужна.
fn secure(proxy: &IUnknown) -> windows::core::Result<()> {
    // SAFETY: `proxy` is a live interface; a null principal name and null auth info mean "the process identity".
    let done = unsafe {
        CoSetProxyBlanket(
            Some(proxy),
            RPC_C_AUTHN_WINNT,
            RPC_C_AUTHZ_NONE,
            PCWSTR::null(),
            RPC_C_AUTHN_LEVEL_CALL,
            RPC_C_IMP_LEVEL_IMPERSONATE,
            None,
            EOAC_NONE,
        )
    };
    match done {
        Err(e) if e.code() == E_NOINTERFACE => Ok(()),
        other => other,
    }
}

/// Значение свойства объекта WMI.
fn get(object: &IWbemClassObject, name: &str) -> Result<Var, HwError> {
    let mut value = Var(VARIANT::default());
    // SAFETY: the name is a live BSTR; `value.0` is an empty VARIANT that the call fills and Var clears on drop.
    unsafe { object.Get(&BSTR::from(name), 0, &raw mut value.0, None, None) }
        .map_err(|e| HwError(format!("поле {name} не прочитано: {}", why(&e))))?;
    Ok(value)
}

/// Записывает свойство объекта WMI; объект копирует значение себе.
fn put(object: &IWbemClassObject, name: &str, value: &Var) -> windows::core::Result<()> {
    // SAFETY: the name is a live BSTR and `value.0` a valid VARIANT, both outliving the call; type 0 = the
    // property's own type.
    unsafe { object.Put(&BSTR::from(name), 0, &raw const value.0, 0) }
}

/// Нет пространства или класса — интерфейса на машине нет (не HP); иначе — `what` с причиной.
fn absent_or(e: &windows::core::Error, what: &str) -> HwError {
    let code = e.code().0;
    if code == WBEM_E_INVALID_NAMESPACE.0 || code == WBEM_E_INVALID_CLASS.0 {
        return HwError(ABSENT.into());
    }
    HwError(format!("{what}: {}", why(e)))
}

/// Причина ошибки COM для журнала: текст Windows, если он есть, и код.
fn why(e: &windows::core::Error) -> String {
    let text = e.message();
    let code = e.code().0;
    if text.trim().is_empty() {
        format!("код 0x{code:08X}")
    } else {
        format!("{} (код 0x{code:08X})", text.trim())
    }
}

/// VARIANT, который сам освобождает то, что держит (строку, массив, объект).
struct Var(VARIANT);

impl Drop for Var {
    fn drop(&mut self) {
        // SAFETY: the VARIANT is either empty, built by the constructors below (it owns its array or reference), or
        // filled by WMI's Get (the caller owns the result).
        let cleared = unsafe { VariantClear(&raw mut self.0) };
        // Отказ возможен только для испорченного VARIANT, а такие здесь не строятся; сообщить о нём из Drop некуда.
        let _ = cleared;
    }
}

impl Var {
    fn with(vt: VARENUM, value: VARIANT_0_0_0) -> Self {
        Self(VARIANT {
            Anonymous: VARIANT_0 {
                Anonymous: ManuallyDrop::new(VARIANT_0_0 {
                    vt,
                    wReserved1: 0,
                    wReserved2: 0,
                    wReserved3: 0,
                    Anonymous: value,
                }),
            },
        })
    }

    /// 32-битное число: так WMI принимает свойства uint32.
    fn int(value: i32) -> Self {
        Self::with(VT_I4, VARIANT_0_0_0 { lVal: value })
    }

    /// Встроенный объект WMI (свойство типа object): VARIANT держит свою ссылку на него.
    fn object(value: &IWbemClassObject) -> Self {
        let unknown = IUnknown::from(value.clone());
        Self::with(
            VT_UNKNOWN,
            VARIANT_0_0_0 {
                punkVal: ManuallyDrop::new(Some(unknown)),
            },
        )
    }

    /// Массив байтов (свойство uint8[]).
    fn bytes(data: &[u8]) -> Result<Self, HwError> {
        let len = u32::try_from(data.len()).map_err(|e| HwError(e.to_string()))?;
        // SAFETY: creates a new one-dimensional byte array; no pointers are passed.
        let array = unsafe { SafeArrayCreateVector(VT_UI1, 0, len) };
        if array.is_null() {
            return Err(HwError("нет памяти под массив байтов".into()));
        }
        // С этой строки массивом владеет VARIANT: на любом выходе ниже его уничтожит VariantClear в Var::drop.
        let var = Self::with(VT_ARRAY | VT_UI1, VARIANT_0_0_0 { parray: array });
        if data.is_empty() {
            return Ok(var);
        }
        let mut target: *mut c_void = null_mut();
        // SAFETY: `array` is the live array created above; `target` is writable and receives the address of its
        // `len` bytes, valid until SafeArrayUnaccessData.
        unsafe { SafeArrayAccessData(array, &raw mut target) }
            .map_err(|e| HwError(format!("массив байтов не заполнен: {}", why(&e))))?;
        // SAFETY: `target` points at `len` writable bytes of the locked array (element size 1); it cannot overlap
        // `data`, which this function only reads.
        unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), target.cast::<u8>(), data.len()) };
        // SAFETY: balances the successful SafeArrayAccessData above on the same array.
        unsafe { SafeArrayUnaccessData(array) }
            .map_err(|e| HwError(format!("массив байтов не заполнен: {}", why(&e))))?;
        Ok(var)
    }

    /// Тег и значение. Каждый VARIANT начинается с этой части: у ветки DECIMAL тег лежит на том же месте (oaidl.h).
    fn part(&self) -> &VARIANT_0_0 {
        // SAFETY: reading the tagged view of a VARIANT is valid for every VARIANT, see above; the reference lives no
        // longer than `self`.
        unsafe { &self.0.Anonymous.Anonymous }
    }

    /// Число uint32 (WMI отдаёт его как `VT_I4`); None — значение другого типа или пустое.
    fn uint(&self) -> Option<u32> {
        let part = self.part();
        if part.vt == VT_I4 {
            // SAFETY: the tag says the value is lVal.
            return Some(unsafe { part.Anonymous.lVal }.cast_unsigned());
        }
        if part.vt == VT_UI4 {
            // SAFETY: the tag says the value is ulVal.
            return Some(unsafe { part.Anonymous.ulVal });
        }
        None
    }

    /// Строка (копия); None — значение другого типа.
    fn text(&self) -> Option<BSTR> {
        let part = self.part();
        if part.vt != VT_BSTR {
            return None;
        }
        // SAFETY: the tag says the value is bstrVal; it is cloned, the original stays with the VARIANT.
        let text = unsafe { &part.Anonymous.bstrVal };
        Some(BSTR::clone(text))
    }

    /// Встроенный объект WMI; None — значение другого типа, пустое или не объект WMI.
    fn object_of(&self) -> Option<IWbemClassObject> {
        let part = self.part();
        if part.vt != VT_UNKNOWN {
            return None;
        }
        // SAFETY: the tag says the value is punkVal (a reference or null); it is borrowed, not taken.
        let unknown = unsafe { &part.Anonymous.punkVal };
        // Не тот интерфейс — значит, это не объект WMI; вызывающий скажет, какого поля не хватило.
        Option::as_ref(unknown)?.cast().ok()
    }

    /// Копия одномерного массива байтов; None — значение другого типа или массив не читается.
    fn bytes_of(&self) -> Option<Vec<u8>> {
        let part = self.part();
        if part.vt != (VT_ARRAY | VT_UI1) {
            return None;
        }
        // SAFETY: the tag says the value is a SAFEARRAY pointer owned by this VARIANT.
        let array = unsafe { part.Anonymous.parray };
        if array.is_null() {
            return None;
        }
        // SAFETY: `array` is a live SAFEARRAY owned by this VARIANT.
        if unsafe { SafeArrayGetDim(array) } != 1 {
            return None;
        }
        // Границы не прочитались — массив испорчен; вызывающий назовёт ответ пустым.
        // SAFETY: as above; dimension 1 exists.
        let low = unsafe { SafeArrayGetLBound(array, 1) }.ok()?;
        // SAFETY: as above.
        let high = unsafe { SafeArrayGetUBound(array, 1) }.ok()?;
        let len = usize::try_from(i64::from(high) - i64::from(low) + 1).unwrap_or(0);
        if len == 0 {
            return Some(Vec::new());
        }
        let mut source: *mut c_void = null_mut();
        // SAFETY: `array` is live; `source` is writable and receives the address of its elements, valid until
        // SafeArrayUnaccessData.
        unsafe { SafeArrayAccessData(array, &raw mut source) }.ok()?;
        let copy = if source.is_null() {
            None
        } else {
            // SAFETY: the element type is VT_UI1 (size 1) and the array holds `len` elements, locked by the access
            // above; the slice is copied before the lock is released.
            Some(unsafe { std::slice::from_raw_parts(source.cast::<u8>(), len) }.to_vec())
        };
        // SAFETY: balances the successful SafeArrayAccessData above on the same array.
        let unlocked = unsafe { SafeArrayUnaccessData(array) };
        // Снять блокировку после удачной не может не выйти; копия уже сделана, и данным это не вредит.
        let _ = unlocked;
        copy
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ответ прошивки на 0x2D в том виде, в каком его отдаёт метод `hpqBIOSInt128`: 128 байт.
    fn answer_of(cpu: u8, gpu: u8) -> [u8; 128] {
        let mut answer = [0u8; 128];
        answer[0] = cpu;
        answer[1] = gpu;
        // Третий байт — третий вентилятор у моделей, где он есть (один источник); у 8D41 его нет, и он не читается.
        answer[2] = 77;
        answer
    }

    #[test]
    fn answer_gives_processor_fan_then_graphics_fan_in_hundreds() {
        // 14.10 совместный замер записал около 2860 / 3060 об/мин при выключенном максимуме.
        assert_eq!(decode(&answer_of(29, 31)), Ok(vec![2900, 3100]));
    }

    #[test]
    fn stopped_fans_are_a_reading_not_an_error() {
        assert_eq!(decode(&answer_of(0, 0)), Ok(vec![0, 0]));
    }

    #[test]
    fn short_answer_is_an_error() {
        for short in [&[][..], &[30][..]] {
            let Err(HwError(text)) = decode(short) else {
                panic!("ответ из {} байт принят за обороты", short.len());
            };
            assert!(text.contains("не того размера"), "{text}");
        }
    }

    #[test]
    fn readings_of_14_10_above_the_limit_are_impossible_values() {
        // Сырые ответы 14.10 из data\error.log: 153/154, 165/167, 128/128.
        for (cpu, gpu) in [(153, 154), (165, 167), (128, 128)] {
            let Err(HwError(text)) = decode(&answer_of(cpu, gpu)) else {
                panic!("{cpu}/{gpu} принято за обороты");
            };
            assert!(text.contains("невозможное значение"), "{text}");
        }
    }

    #[test]
    fn limit_is_9000_rpm_inclusive() {
        assert_eq!(decode(&answer_of(90, 90)), Ok(vec![9000, 9000]));
        assert!(decode(&answer_of(91, 30)).is_err());
    }

    #[test]
    fn one_impossible_fan_rejects_the_whole_reading() {
        assert!(decode(&answer_of(30, 120)).is_err());
        assert!(decode(&answer_of(120, 30)).is_err());
    }

    #[test]
    fn simulation_returns_what_it_holds_or_fails() {
        let mut fans = SimFans {
            rpm: vec![2400, 2600],
            fail: false,
        };
        assert_eq!(fans.rpm(), Ok(vec![2400, 2600]));
        assert_eq!(fans.rpm(), Ok(vec![2400, 2600]), "второй опрос — то же");
        fans.fail = true;
        assert!(fans.rpm().is_err());
    }

    #[test]
    fn simulation_applies_the_same_limit() {
        let mut fans = SimFans {
            rpm: vec![15_300, 15_400],
            fail: false,
        };
        let Err(HwError(text)) = fans.rpm() else {
            panic!("невозможные обороты имитации приняты");
        };
        assert!(text.contains("невозможное значение"), "{text}");
    }

    // Массив байтов в VARIANT — так подпись и данные уходят в WMI, а ответ приходит обратно. Нужен только oleaut32,
    // не WMI и не прошивка.
    #[test]
    fn byte_array_survives_the_variant() {
        let Ok(var) = Var::bytes(&SIGN) else {
            panic!("массив не создан");
        };
        assert_eq!(var.bytes_of(), Some(SIGN.to_vec()));
        let Ok(empty) = Var::bytes(&[]) else {
            panic!("пустой массив не создан");
        };
        assert_eq!(empty.bytes_of(), Some(Vec::new()));
        assert_eq!(Var::int(5).bytes_of(), None, "число не массив");
    }

    #[test]
    fn uint32_comes_back_from_either_tag() {
        assert_eq!(Var::int(0).uint(), Some(0));
        // Код ответа — uint32: большой код приходит отрицательным VT_I4 и читается обратно без потерь.
        assert_eq!(Var::int(-1).uint(), Some(u32::MAX));
        assert_eq!(Var::with(VT_UI4, VARIANT_0_0_0 { ulVal: 7 }).uint(), Some(7));
        assert_eq!(Var(VARIANT::default()).uint(), None, "пустое значение");
    }
}
