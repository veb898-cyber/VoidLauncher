# Liquid Glass — Technical Research

> **Назначение документа.** Технический справочник для будущего coding agent'а, который будет проектировать и реализовывать визуально правдоподобный Liquid Glass в VoidLauncher (Tauri 2 + React 19 + WebView2/Windows). Документ **не содержит кода** и не описывает конкретную реализацию — он фиксирует модель материала, ограничения среды и правила, по которым потом проектируется решение.
>
> **Строгое правило доказательности.** Каждое утверждение помечено:
> - **FACT** — дословно подтверждено официальной документацией Apple (HIG, WWDC-транскрипт, API-документация, Technology Overviews).
> - **OBSERVATION** — наблюдаемое поведение продуктов Apple (визуальный результат без раскрытого алгоритма).
> - **INFERENCE** — инженерный вывод, логически следующий из доступных данных. **Не** является подтверждённым фактом.
> - **RECOMMENDATION** — рекомендация для VoidLauncher.
>
> Если внутренний алгоритм Apple не раскрыт — это указано явно, без домысливания.

**Дата исследования:** 2026-09-27
**Платформы Apple:** iOS 26, iPadOS 26, macOS Tahoe 26, watchOS 26, tvOS 26, visionOS 26, Mac Catalyst 26
**Целевая платформа проекта:** Windows 10/11, Tauri 2, WebView2 (Edge/Chromium)

---

## 1. Executive Summary

Liquid Glass — **не** «полупрозрачный фон с blur». Это цифровая мета-материя, которая **динамически преломляет, формирует и концентрирует свет** (термин Apple — **Lensing**), адаптируясь к содержимому под собой, своему размеру, окружению и взаимодействию.

**Три фундаментальных факта, определяющих всё остальное (все FACT, WWDC25-219):**

1. **Lensing, а не рассеивание.** «Whereas previous materials scattered light, this new set of materials dynamically bends, shapes, and concentrates light in real time.» Прежние материалы *рассеивали* свет; Liquid Glass *преломляет, формирует и концентрирует* его. Именно преломление, а не размытие, создаёт оптическую толщу и ощущение стекла.
2. **Материал адаптируется по слоям и по размеру.** «Liquid Glass is composed of a number of layers… each layer continuously adapts based on what's behind it.» Плюс: при увеличении элемента «its material characteristics change to simulate a thicker, more substantial material. It casts deeper, richer shadows, has more pronounced lensing and refraction effects, and a softer scattering of light.» То есть **большой элемент — это физически более толстое стекло**, а не просто растянутое маленькое.
3. **Стекло появляется не через прозрачность, а через модуляцию преломления.** «Instead of fading, Liquid Glass objects materialize in and out by gradually modulating the light bending and lensing, ensuring a graceful transition that preserves the optical integrity of the material.» Появление/исчезновение — через изменение силы lensing, **не** через `opacity`.

**Практический вывод для Web (INFERENCE + RECOMMENDATION):** материал, построенный только на `backdrop-filter: blur()` + полупрозрачном фоне, систематически **не будет** выглядеть как Liquid Glass, потому что в нём нет преломления и нет адаптации к фону. Ближайшее достижимое в браузере приближение — **Tier 2** (CSS: blur + адаптивный tint + корректная геометрия + edge-обработка + общий контейнер), а Tier 3 (WebGL-шейдер с screen-space displacement) — единственный путь к настоящей «стеклянности».

---

## 2. Что такое Liquid Glass: официальное определение

### 2.1 FACT — исходные формулировки Apple

| Источник | Формулировка |
|---|---|
| WWDC25-219 «Meet Liquid Glass» | «a new digital meta-material that dynamically bends and shapes light»; «it behaves and moves organically in a manner that feels more like a lightweight liquid» |
| WWDC25-323 «Build a SwiftUI app with the new design» | «a brand new, adaptive material for controls and navigational elements that we call Liquid Glass. It takes inspiration from the optical properties of glass and the fluidity of liquid» |
| WWDC25-356 «Get to know the new design system» | «Liquid Glass defines a new functional layer in the UI, floating above your content to bring structure and clarity, without ever stealing focus» |
| Technology Overviews «Adopting Liquid Glass» | «a new dynamic material called Liquid Glass, which combines the optical properties of glass with a sense of fluidity. This material forms a distinct functional layer for controls and navigation elements» |
| SwiftUI `Applying Liquid Glass to custom views` | «a material that blurs content behind it, reflects color and light of surrounding content, and reacts to touch and pointer interactions in real time» |
| SwiftUI `glassEffect(_:in:)` | система «Renders a shape anchored behind a view with the Liquid Glass material» + «Applies the foreground effects of Liquid Glass over a view» |

**FACT:** Liquid Glass — **функциональный слой** UI (controls + navigation), плавающий **над контентом**. Материал объединяет: (а) размытие содержимого за собой, (б) отражение цвета и света окружения, (в) реактивность на касание и указатель в реальном времени.

**FACT — предыстория (важно для отличия от старого blur).** WWDC25-219 прямо прослеживает линию: «It builds on learnings from all the way from the Aqua user interface of Mac OS X, through to the realtime blurs of iOS 7, to the fluidity of iPhone X, the flexibility of the Dynamic Island, and the immersive interface of visionOS. Building off these learnings, **rather than trying to simply recreate a material from the physical world**, Liquid Glass is a new digital meta-material.»

Ключевое: Apple **намеренно не воспроизводит** физический материал, а создаёт цифровую мета-материю. Значит, искать в ней физическую точность незачем — нужно искать **принципы**.

### 2.2 Какую проблему решает

**FACT (WWDC25-356):** «Liquid Glass defines a new functional layer in the UI, floating above your content to bring structure and clarity, **without ever stealing focus**.» Проблема: UI должен отделяться от контента, но не перетягивать внимание.

**FACT (Adopting Liquid Glass):** «It affects how the interface looks, feels, and moves, adapting in response to a variety of factors to help bring focus to the underlying content. Liquid Glass seeks to bring attention to the underlying content, and **overusing this material in multiple custom controls can provide a subpar user experience by distracting from that content.** Limit these effects to the most important functional elements in your app.»

**FACT (WWDC25-356):** «Previously, they would blend into the background when the interface was at rest. Liquid Glass helps lift them, creating more separation from your content while reinforcing interactivity.»

Три задачи:
1. **Отделить** навигацию/контролы от контента.
2. **Не отвлекать** от контента (материал подчинён контенту, а не наоборот).
3. **Не терять элементы** в покое (в отличие от старых полупрозрачных панелей, сливавшихся с фоном).

### 2.3 Свойства: специфичные для Liquid Glass vs общие принципы дизайн-системы

Это важное разграничение, иначе будущий агент скопирует в VoidLauncher лишнее.

| Свойство | Специфично для Liquid Glass | Общий принцип новой дизайн-системы Apple |
|---|---|---|
| Lensing (преломление/концентрирование света) | **ДА** | — |
| Backdrop sampling + адаптация слоёв к фону | **ДА** | — |
| Адаптация материала к размеру (больше = толще) | **ДА** | — |
| Размер-зависимое переключение light/dark | **ДА** | — |
| Адаптивная ощупа (tint, меняющий hue/brightness/saturation по фону) | **ДА** | — |
| Интерактивное «просвечивание изнутри» (illumination from within) | **ДА** | — |
| Материализация через модуляцию преломления, а не fade | **ДА** | — |
| Запрет glass-on-glass | **ДА** (материал-специфичное правило) | — |
| Два неперемешиваемых варианта: Regular / Clear | **ДА** | — |
| **Concentricity** (концентричность радиусов) | — | **ДА** (geometry-принцип, применим к любой форме) |
| **Scroll edge effects** (soft / hard) | — | **ДА** (работает в связке с материалом) |
| **Capsule / fixed / concentric** формы | — | **ДА** (типология форм) |
| `backgroundExtensionEffect` (зеркалирование контента за safe area) | — | **ДА** (композиционный приём) |
| Grouping, иерархия вместо декора | — | **ДА** |
| Monochrome icons, выравнивание по leading | — | **ДА** |
| Continuity / shared anatomy | — | **ДА** |
| `ToolbarSpacer` для группировки | — | **ДА** |

**Вывод для VoidLauncher (RECOMMENDATION):** переносить **геометрические и композиционные** принципы (concentricity, scroll edge effect, grouping, «иерархия вместо декора») **можно и нужно** — они не требуют стекла. Оптические (lensing, tint-адаптация, size-зависимая толщина) требуют рендера и должны быть Tier-gated.

---

## 3. Liquid Glass vs Glassmorphism и старые материалы

### 3.1 Сравнительная таблица

| Характеристика | Старый blur/vibrancy (iOS 7, `UIBlurEffect`, `NSVisualEffectView`) | Glassmorphism (web-тренд) | **Liquid Glass** |
|---|---|---|---|
| Оптическое ядро | Рассеивание (scatter) | **Нет** — только blur + полупрозрачность | **Преломление/концентрирование (lensing)** — «bends, shapes, and concentrates light in real time» |
| Адаптация к фону | Фиксированная (светлая/тёмная задана заранее) | Обычно статичная, «приклеена» к теме | **Каждый слой непрерывно адаптируется**; независимое переключение light/dark |
| Адаптация к размеру | Нет | Нет | **Да** — большой элемент = более толстое стекло, глубже тени, сильнее преломление |
| Реакция на взаимодействие | Нет | Почти нет | **Да** — flex, energize, «illuminates from within», scale/bounce/shimmer |
| Refraction/distortion | Нет | Обычно нет | **Да** (lensing + refraction, оба термина Apple) |
| Behaviour при появлении | Fade | Fade | **Модуляция преломления**, fade ломает оптическую целостность |
| Отражение цвета окружения | Vibrancy (цвет текста) | Нет | **Да** — «reflects color and light of surrounding content»; насыщенный контент «spill»-ится на поверхность |
| Объединение соседних элементов | Нет | Нет | **Да** — `GlassEffectContainer`, shared sampling region, morphing |
| Морфинг форм | Нет | Почти нет | **Да** — `glassEffectID`, `glassEffectUnion`, `GlassEffectTransition` |
| Тень адаптивна к фону | Нет | Обычно фиксированная | **Да** — тень темнее над текстом, светлее над сплошным светлым фоном |
| Переключение light/dark по размеру | — | — | **Размер-зависимое:** мелкие элементы (nav/tab bar) переключаются; крупные (меню, сайдбары) — **не** переключаются |
| Проблема с вложенностью | — | — | **Glass-on-glass запрещён** (артефакты + визуальный мусор) |

### 3.2 Почему «полупрозрачный фон + blur» не является Liquid Glass

**INFERENCE, опирающееся на FACT из WWDC25-219:** Apple прямо противопоставляет рассеивание и преломление. Полупрозрачный фон + blur — это *рассеивание* (frosted). Три конкретные причины, почему blur-only ломает материал:

1. **Нет направленного преломления.** Blur размазывает задник изотропно. Lensing же **искривляет** изображение — видны искажения у скруглений и «линзование» содержимого. Глаз читает искажение как оптическую толщину.
2. **Нет адаптации к фону.** Статический `rgba()` не меняется, когда под панелью проезжает тёмный лог или светлый скриншот. У LG каждый слой подстраивается, чтобы сохранить контраст.
3. **Нет тени, зависящей от подложки.** У LG тень над текстом плотнее, чем над ровным фоном. Статильная тень этого не даёт.

---

## 4. Оптическая модель материала

Ниже — разбор каждого наблюдаемого свойства в формате: **что видно → зачем → физика → как в CG → что доступно в браузере → что невозможно → чем заменить.**

### 4.1 Lensing (преломление) — центральное свойство

**Что видно (OBSERVATION):** содержимое под материалом не просто размыто — оно **искривлено** у краёв. Высокий контрастный объект (текст лога, иконка, граница карточки) рядом с панелью выглядит «выгнутым» или «вдавленным» вблизи скруглений. Ближе к центру эффект слабее.

**Зачем (FACT, WWDC25-219):** «Liquid Glass uses these instinctive visual cues to provide separation and communicate layering in a new way while letting content shine through underneath it.» → Lensing одновременно (а) отделяет материал от фона, (б) даёт оптическую толщину, (в) позволяет контенту просвечивать.

**Физика (INFERENCE):** луч света, идя через границу двух сред (воздух → стекло), отклоняется. На плоской границе отклонение мало; на **скруглённой/криволинейной границе** нормаль поверхности меняется быстро, и отклонение максимально на скруглении. Отсюда: искажение концентрируется у кромок, а не в центре.

**Как в CG:** экранный шейдер, вычисляющий нормаль/градиент поля формы, вычисляет вектор преломления и смещает UV семпла. Практически — displacement-карта, построенная из SDF формы.

**В браузере:**

| Способ | Реалистичность | Комментарий |
|---|---|---|
| CSS | **Невозможно** | Нет API для искажения backdrop |
| `backdrop-filter` | **Невозможно** | Только blur + цветовые функции (FACT: MDN — filter-function list = blur, brightness, contrast, drop-shadow, grayscale, hue-rotate, invert, opacity, saturate, sepia) |
| SVG `feDisplacementMap` | Ограниченно | Работает, но применяется к элементу, а не к «экрану позади»; нужен явный источник |
| WebGL fragment shader | **Да, полноценно** | Правильный путь: sample регион → смещение UV по нормали → blur |
| WebGPU | Да | То же, современный API |

**Чем заменить, если WebGL недоступен (RECOMMENDATION):** не пытаться имитировать искажение. Вместо этого компенсировать **edge-обработкой** (мягкий внутренний градиент, тонкий rim highlight) и **адаптивным tint** — это даёт 70% ощущения «стекла» без искажения контента, который часто нужно читать.

### 4.2 Blur (размытие)

**Что видно:** содержимое под материалом теряет детализацию, но узнаваемость сохраняется.
**Зачем (FACT):** «blurs content behind it» — снижение шума, повышение контраста текста поверх.
**Физика:** рассеяние. **В CG:** Gaussian/dual-kawase blur.
**В браузере:** `backdrop-filter: blur(Npx)` — прямой аналог, GPU-ускоренный.
**Важно (INFERENCE):** blur сам по себе — анти-дисторшн-эффект: он маскирует мелкие ошибки композиции, но не создаёт стекло.

### 4.3 Адаптация к фону (backdrop responsiveness)

**Что видно (FACT, WWDC25-219):** панель меняет вид в зависимости от того, что под ней:
- «As text scrolls underneath, shadows become more prominent to create additional separation.»
- «The amount of tint and the dynamic range shift to always ensure buttons remain legible, while letting as much of the content through as possible.»
- «it can also independently switch between light and dark, allowing it to feel perfectly at home in any context»
- «On larger elements, like sidebars, the appearance of Liquid Glass is informed by the ambient environment within the app. Light from colorful content nearby can subtly spill onto its surface»
- «the light reflects, scatters, and bleeds into the shadow as well»

**Ключевой подтверждённый механизм (FACT):** **размер-зависимое** поведение:
> «Small elements like navbars and tabbars, constantly adapt their appearance depending on what’s behind them. They also **flip from light to dark** based on the background… **Bigger elements, like menus or sidebars also adapt based on context, but they don’t flip from light to dark. Their surface area is too big and transitions like these would be distracting.**»

То есть: **маленький элемент — контрастно переключается; большой — адаптирует прозрачность/тон, но не переключает полярность.** Это прямо применимо к дизайну: sidebar в macOS не «инвертируется», но его фон подстраивается.

**FACT — про текст/глифы поверх стекла:** «To maintain legibility, symbols and glyphs on top of Liquid Glass, do the same. They flip from light to dark and vice versa, **mirroring the glass’s behavior** to maximize contrast. All content placed on the Regular variant will automatically receive this treatment.»

То есть **foreground повторяет поведение фона** — если фон посветлел, иконки темнеют. Это ключ к «живому» ощущению.

**В браузере:**
- CSS: **нет** автоматической адаптации. Только `color-mix()`, `filter: invert()` (грубо, ломает цвет) или собственная логика на JS (вычисление средней яркости подложки).
- **INFERENCE:** для VoidLauncher реалистичен подход «вычислять яркость подложки и переключать набор CSS-переменных» (themer, уже есть `themeStore`).
- WebGL: адаптация становится естественной (luminance pass по сэмплированному backdrop).

### 4.4 Адаптивная ощупка (Tint)

**FACT (WWDC25-219):** «Liquid Glass introduces a new way of tinting elements that respects the principles of the material and maximizes legibility… **Selecting a color generates a range of tones that are mapped to content brightness underneath the tinted element.** It draws inspiration from how colored glass works in reality: **changing its hue, brightness and saturation depending on whats behind without deviating too much from the intended color.**»

**FACT:** ощупка совместима со всеми поведениями материала; tint применяется только для смыслового акцента («Tinting should only be used to bring emphasis to primary elements and actions»), и **не** ко всем элементам («Avoid tinting all your elements. When every element is tinted, nothing stands out»).

**В браузере:** `color-mix()` + `--accent` (в проекте уже есть `--accent`, `--accent-dim`) → грубая, но рабочая аппроксимация. Тон-по-яркости-фона — только вручную/JS или в шейдере.

### 4.5 Specular highlights и edge lighting

**FACT (WWDC25-219):** «the highlights layer Liquid Glass lives inside an environment that behaves like the world around us. Light sources inside of this environment shine on the material producing **highlights that respond to geometry just as you’d expect**. On interactions, such as locking and unlocking your phone, **these lights move in space, causing light to travel around the material, defining its silhouette**. And in some cases, the lighting responds to device motion, making it feel like Liquid Glass is aware of its position in the real world.»

**Что видно:** световая кромка обходит силуэт элемента; она ярче на выпуклых/скруглённых участках и тускнеет там, где поверхность «уходит».
**Зачем (FACT):** «defining its silhouette» — задаёт читаемый контур и передаёт объём.
**Физика:** отражение ЛИ источника на гладкой поверхности. На плоской грани — блик; на скруглении — блик тянется вдоль кромки.
**В браузере:** статический аналог — тонкий градиентный rim (`::after` с `linear-gradient` + `mask`), `mix-blend-mode: screen` для аддитивности. **Динамический** (свет, «обтекающий» элемент по геометрии, реагирующий на движение устройства) — **невозможен в CSS**; требует шейдера или хотя бы позиционированного источника света, привязанного к геометрии.
**RECOMMENDATION для десктопа:** в статичном desktop-приложении (мышь не наклоняет устройство) динамический specular почти не нужен. Достаточно статического rim, ориентированного на типичное положение окна (сверху-слева — как источник).

### 4.6 Адаптивная тень

**FACT (WWDC25-219):** «The element is aware of what’s behind it and **increases the opacity of its shadow when it is over text. Conversely, it lowers the opacity of its shadow when it is over a solid light background.** This provides separation from the content to make sure elements are always easy to spot.»

**В браузере:** **возможно** — через CSS-переменную, значение которой вычисляется по яркости/плотности подложки (JS или `color-mix` с фоном). Статическая тень — хуже, но не катастрофа.

### 4.7 Оптическая толщина, зависящая от размера

**FACT (WWDC25-219):** «When glass flexes and morphs to larger sizes – like when presenting a menu from a toolbar button – **its material characteristics change to simulate a thicker, more substantial material. It casts deeper, richer shadows, has more pronounced lensing and refraction effects, and a softer scattering of light.** These subtle changes enhance the perceived depth and aid in the legibility of the content within the glass element itself.»

**INFERENCE для Web:** вводя «толщину» как параметр размера (например, 0–1, где 1 = крупный панельный элемент), можно масштабировать: blur, глубину тени, силу edge-градиента, насыщенность tint, интенсивность внутреннего свечения. Это дешёвый и очень заметный способ сделать материал правдоподобнее: **маленький элемент — «тонкое стекло», большой — «толстое»**.

### 4.8 Illumination from within (интерактивное свечение)

**FACT (WWDC25-219):** «When you interact with Liquid Glass, **the material illuminates from within as a form of feedback. Starting right under your fingertips, the glow spreads throughout the element and onto any Liquid Glass elements nearby**, interacting with the flexible properties of the material in a way that feels natural and fluid.»

**FACT (WWDC25-323):** «Glass reacts to user interaction by **scaling, bouncing, and shimmering**».

**В браузере:** полностью воспроизводимо **радиальным градиентом**, позиционированным в точке курсора (`--mx`, `--my` из pointer-событий), с transition по размеру/прозрачности. **Это одно из самых дешёвых и самых заметных улучшений** — рекомендую как обязательный элемент Tier 2.

### 4.9 Остальные наблюдаемые свойства

| Свойство | Категория | Комментарий |
|---|---|---|
| Прозрачность/полупрозрачность | FACT | Адаптивная; варьируется по контексту |
| Динамическое изменение при движении | FACT | Материал реагирует на движение устройства (неприменимо к десктопу) |
| Изменение материала при перемещении | FACT | Morph между состояниями |
| Объединение соседних glass-областей | FACT | `GlassEffectContainer` |
| Recede при потере фокуса окна | FACT | «when a window loses focus on the Mac or iPad, Liquid Glass shifts its appearance and visually recedes to guide attention» — **прямо релевантно для desktop-приложения!** |
| Materialize через модуляцию преломления | FACT | Не fade |
| Реакция на содержимое под стеклом | FACT | Адаптация слоёв |
| Реакция при прокрутке | FACT | Scroll edge effects |
| Elastic properties (податливость) | FACT | Отключается при Reduced Motion |

---

## 5. Materialize, morph и поведение границ

### 5.1 Появление/исчезновение (FACT, WWDC25-219)

«Instead of fading, Liquid Glass objects materialize in and out by **gradually modulating the light bending and lensing**, ensuring a graceful transition that preserves the optical integrity of the material.»

**INFERENCE для Web:** появление через `opacity: 0→1` — самая распространённая ошибка, потому что она **визуально противоречит** модели материала: полностью прозрачный слой не имеет преломления, значит, исчезает и оптическая толщина, и контур. Правильнее: держать элемент непрозрачным по `opacity`, а **варьировать blur/tint/rim** — тогда исчезновение выглядит как «остывание стекла», а не как проявление картинки.

### 5.2 Glass-on-glass запрещён (FACT, WWDC25-219)

«**always avoid glass on glass.** Stacking Liquid Glass elements on top of each other can quickly make the interface feel cluttered and confusing. When placing elements on top of Liquid Glass, **avoid applying the material to both layers. Instead, use fills, transparency, and vibrancy for the top elements to make them feel like a thin overlay that is part of the material.**»

Это **прямое техническое следствие** правила «glass cannot sample other glass» (раздел 6.2): вложенное стекло семплит не то, что нужно, и Apple фиксирует это как запрещённый паттерн, а не как поддерживаемую комбинацию.

**RECOMMENDATION для VoidLauncher:** если модалка лежит поверх sidebar — sidebar должен быть **не** glass в момент открытия модалки, либо модалка должна быть fill/vibrancy, а не отдельным стеклом. Композиция «glass + scrim + glass» — самая частая ошибка.

### 5.3 Glass в content layer (FACT, WWDC25-219)

«It may be tempted to use Liquid Glass everywhere but it is best reserved for the navigation layer that floats above the content of your app. Consider this tableview: making it Liquid Glass would make it compete with other elements and muddy the hierarchy. So keep it in the content layer instead to ensure clarity.»

Дополнительно (FACT, WWDC25-219): «In steady states, such as when an app first launches, **avoid intersections between content and Liquid Glass**. Instead, **reposition or scale the content to maintain separation**.»

**INFERENCE:** контент не должен «пересекать» материал в статичном состоянии — либо отодвигается, либо масштабируется. Это объясняет, почему в iOS 26 контент уходит в safe area, а не просто «лежит под панелью».

### 5.4 Два варианта материала (FACT, WWDC25-219)

> «There are two to choose from: Regular and Clear. **They should never be mixed**, as they each have their own characteristics and specific use cases.
> **Regular** is the most versatile and the one you will be using the most. This variant gives you all the visual and adaptive effects we’ve talked about, and provides legibility regardless of context. It works in any size, over any content and anything can be placed on top of it.
> **Clear**, on the other hand, **does not have adaptive behaviors**. It is permanently more transparent… **To provide enough legibility for symbols or labels, it needs a dimming layer to darken the underlying content.** Without it, legibility gets noticeably worse.»

Clear допустим **только** при выполнении **трёх** условий:
1. Элемент расположен **поверх media-rich контента**.
2. Content layer **не пострадает** от затемняющего слоя.
3. Контент **над** ним — жирный и яркий.

**API (FACT):** SwiftUI `Glass`: `.regular`, `.clear`, `.identity`; UIKit `UIGlassEffect.Style` + `isInteractive` + `tintColor`.

**INFERENCE для VoidLauncher:** вариант «Clear» практически недостижим/нежелателен в desktop-лаунчере — контент не media-rich, а текст мелкий. Значит: **использовать только Regular-подобное поведение**, и это сильно упрощает задачу.

---

## 6. Blur, backdrop sampling и контейнеры

### 6.1 Два слоя материала (FACT, SwiftUI `glassEffect`)

API doc явно делит эффект на две части:
1. «Renders a shape anchored **behind a view** with the Liquid Glass material» — задний слой.
2. «Applies the **foreground effects** of Liquid Glass **over** a view» — передний слой.

Также: «SwiftUI **anchors the Liquid Glass to a view’s bounds**… the material fills the entirety of the Text frame, which includes the padding.» → материал привязан к bounds включая padding; по умолчанию форма — `Capsule`, вариант — `.regular`.

**INFERENCE для Web:** двуслойная модель напрямую транслируется в DOM: **backing layer** (backdrop-filter) + **::after overlay** (rim, specular, tint, inner glow) + **content layer** (текст/иконки). Три DOM-слоя — минимальная корректная структура.

### 6.2 Sampling region и запрет sampling other glass (FACT)

Из «Applying Liquid Glass to custom views»:
> «The glass material reflects and refracts lights, picking colors from nearby content. This effect is achieved by **sampling content from an area larger than itself. However, glass can not sample other glass**, so having nearby glass elements in different containers will result in inconsistent behavior. Using a glass container allows these elements to **share their sampling region**, providing a consistent visual result.»

Из `GlassEffectContainer`:
> «Each view with a Liquid Glass effect contributes a shape rendered with the effect to a set of shapes. **SwiftUI renders the effects together**, improving rendering performance and allowing the effects to interact with and morph into one another. As shapes near one another, their paths start to blend into one another. The higher the spacing, the sooner blending begins as the shapes approach each other.»

**FACT:** семплинг берётся **с области больше, чем сам элемент** (т.е. элемент «видит» и своё окружение, не только то, что строго под ним).

**INFERENCE — критическое ограничение для Web:** `backdrop-filter` по спецификации **не умеет** семплить область больше себя. И наоборот — MDN документирует, что он семплит «до ближайшего backdrop root», то есть **только то, что позади на том же слое**. Оба отклонения от Apple — расширенная область и shared region — в CSS недостижимы напрямую.

### 6.3 Backdrop root — критическое ограничение Web (FACT, MDN)

> «The backdrop-filter property applies filter effects to the pixels painted behind an element, **up to the nearest ancestor that is a backdrop root. Content above the backdrop root is not affected.**»

Backdrop root — это элемент, у которого есть **любое** из:
- корень `<html>`
- `filter` ≠ `none`
- `opacity` < 1
- `mask`, `mask-image`, `mask-border` или `clip-path` ≠ `none`
- `backdrop-filter` ≠ `none`
- `mix-blend-mode` ≠ `normal`
- `will-change`, установленное на любое из перечисленных выше свойств

> «if a parent element has `opacity: 0.9`, it becomes a backdrop root and any child's backdrop-filter will **only blur the content between that parent and the child** — not the content behind the parent. This is a **common source of confusion**…»

**RECOMMENDATION (критично для VoidLauncher):** это самая частая причина «`backdrop-filter` не работает / работает не так». Перед вводом glass в компонент — **обязательно проверить цепочку предков** на эти шесть свойств. В частности, `opacity: < 1` и `mask`/`clip-path` на обёртке **убивают** эффект у потомков.

### 6.4 Производительность vs точность (FACT, Apple)

- «**Creating too many Liquid Glass effect containers and applying too many effects to views outside of containers can degrade performance.** Limit the use of Liquid Glass effects onscreen at the same time.» (`Applying Liquid Glass`)
- «**Combine custom Liquid Glass effects to improve rendering performance.** If you apply these effects to custom elements, make sure to combine them using a GlassEffectContainer» (`Adopting Liquid Glass`)
- «Spacing on the container… **The larger the spacing value on the container, the sooner the Liquid Glass effects behind views blend together**» — то есть чем больше `spacing`, тем агрессивнее слияние форм.

**INFERENCE:** `GlassEffectContainer` — это одновременно и **корректность**, и **оптимизация** (единый рендер-проход вместо N). Прямой аналог в Web: **один `backdrop-filter`-слой на контейнер** вместо N на элементах.

---

## 7. Геометрия, форма и композиция соседних элементов

### 7.1 Concentricity (FACT, WWDC25-356)

> «There's a quiet geometry to how our shapes fit together, driven by **concentricity**. By aligning radii and margins around a shared center, shapes can comfortably nest within each other. And to preserve optical balance, **views are mathematically centered when it makes sense— and subtly offset when it doesn't.**
> We use three shape types to build concentric layouts: **fixed** shapes have a constant corner radius. **Capsules** use a radius that's half the height of the container. And **concentric** shapes **calculate their radius by subtracting padding from the parent's**.»

Также: «Glass controls nest perfectly into the rounded corners of windows, **maintaining concentricity throughout the UI**».

API: `.rect(corner: .containerConcentric)`, `ConcentricRectangle`, `UIView.cornerConfiguration`, `UICornerConfiguration`.

**FACT (поведение при неправильной геометрии):** «keep an eye out for corners that feel **too pinched— or flared**. They can create tension and break the sense of balance.»

**INFERENCE — формула вложенного радиуса.** Классика: `r_inner = r_outer − padding`. Проверка: панель `r=20`, padding 6 → вложенный элемент `r=14`. В проекте как раз есть шкала `--radius-sm: 6px … --radius-xl: 20px` — **идеально ложится** на эту формулу (см. раздел 15).

**FACT (capsule в dense UI):** «In dense desktop environments, [capsules] are best used for standout actions. On macOS, Mini, Small, and Medium controls will continue using **rounded rectangles**… Large controls will now use capsule shapes, alongside with the new X-Large size, which **takes advantage of Liquid Glass to provide emphasis**.»

**RECOMMENDATION для VoidLauncher:** лаунчер — плотный desktop-UI. Capsule применим **точечно** (кнопка Play, бейдж запуска), а **не** как универсальная форма. Sidebar/toolbar — rounded-rect.

### 7.2 Поведение нескольких соседних glass-объектов

**FACT (`GlassEffectContainer`):** формы **сливаются**, когда сближаются; «their paths start to blend into one another»; интенсивность зависит от `spacing` контейнера.

**FACT (`Applying Liquid Glass`):** «Customize the spacing on the container to control how the Liquid Glass effects behind views interact… **A spacing value on the container that’s larger than the spacing of an interior HStack, VStack, or other layout container causes Liquid Glass effects to blend together at rest** because the views are too close to each other. Animating views in or out causes the shapes to morph apart or together as the space in the container changes.»

Это очень конкретное и важное правило: **контейнер может «схлопнуть» форму в покое, если элементы ближе, чем spacing контейнера**.

**FACT (`glassEffectUnion(id:namespace:)`):** несколько вью могут **вносить вклад в единый** эффект с одним ID: «combines all effects with a similar shape, Liquid Glass effect, and ID into a single shape».

**FACT (`glassEffectID` + `GlassEffectTransition`):** `matchedGeometry` (по умолчанию) для элементов внутри spacing контейнера; `materialize` — для более далёких. «The system applies more than opacity changes with the available transition types.»

**INFERENCE для Web:** слияние соседних форм в CSS воспроизводимо **только** через общий контейнер с одним эффектом и через морфинг SVG-путей/`clip-path`. Реальное «слияние двух форм в одну оптическую каплю» в CSS невозможно — можно лишь имитировать через `border-radius`-анимацию и общий блир.

### 7.3 Морфинг (FACT)

«As you go between states in an app, **Liquid Glass dynamically morphs between the controls in each context**. This maintains the concept of having a **singular floating plane** that the controls live on. And it makes transitions between different sections of an app feel fluid and seamless, as the controls continually shape shift. When showing a menu, **the bubble simply pops open** to reveal the content contained within. This lightweight, in-line transition keeps everything right where you just tapped. And it communicates a very clear and direct relationship between the button and the content contained within.»

**INFERENCE:** «bubble simply pops open» + «singular floating plane» = **форма не исчезает и не появляется, а расширяется/сжимается на месте**. Прямой аналог в Web: один элемент, анимируемые `width/height/border-radius` (или `transform: scale` + `clip-path`), а не «исчезнуть + появиться в другом месте».

---

## 8. Взаимодействие и движение

### 8.1 Почему интерфейс ощущается «живым» даже со статичным фоном (ключевой вопрос)

**FACT — четыре независимых источника «жизни» (все WWDC25-219, ни один не требует движущегося фона):**

1. **Lensing + specular, реагирующие на геометрию.** Свет «двигается по материалу» в зависимости от **формы**, а не фона. «highlights that respond to geometry just as you’d expect».
2. **Illumination from within при взаимодействии.** Свечение стартует под пальцем/курсором и расходится по элементу и на соседние стеклянные элементы. Это **реакция на ввод**, а не на фон.
3. **Scale / bounce / shimmer** (FACT, WWDC25-323). Кинетическая упругость материала.
4. **Morph при смене состояний** (раздел 7.3) — «continually shape shift».

Плюс: **Materialize через модуляцию преломления** — вход/выход сами по себе «живые», без fade.

**INFERENCE, прямой ответ на вопрос:** статичность фона не мешает, потому что «жизнь» обеспечивается **формами, светом и реакцией на ввод**, а не подвижностью подложки. Контент под стеклом влияет на **читаемость и тон** (адаптация), но на **ощущение жизни** — вторичен. Практический вывод для VoidLauncher: **интерактивный отклик важнее адаптации к фону**, и его можно реализовать полностью в CSS.

### 8.2 Состояния и их проявления

| Состояние | FACT-источник | Проявление |
|---|---|---|
| Rest | WWDC25-219 | «resting state stay visually quiet, while it comes to life on touch» |
| Touch/press | WWDC25-219, 323 | flex, energize, illumination from within, scale/bounce/shimmer |
| Focus (окно) | WWDC25-219 | «when a window loses focus… shifts its appearance and **visually recedes** to guide attention» |
| TV focus | Adopting Liquid Glass | «standard buttons and controls take on a Liquid Glass appearance **when focus moves to them**» |
| Scroll | WWDC25-219 | scroll edge effect растворяет контент, «lifting the glass visually above the moving content» |
| Modality | WWDC25-356 | «When a task interrupts the main flow, **pair Liquid Glass with a dimming layer**» |
| Depth/engagement | WWDC25-356 | «when focus shifts, like dragging a sheet upward, **Liquid Glass subtly recedes, becoming more opaque and gently growing in size** to signal a deeper level of engagement» |

**RECOMMENDATION (desktop-специфично, высокой ценности):** поведение при **потере фокуса окна** (`window.blur`/`focus` в Tauri через `getCurrentWindow().onFocusChanged()`) — прямой аналог «recedes to guide attention», реализуемый через `document`-класс или CSS-переменную. Для лаунчера это особенно уместно: при переключении на другое приложение интерфейс приглушается.

---

## 9. Scroll edge effects и композиция слоёв

### 9.1 FACT — что это

WWDC25-219: «Scroll edge effects work in concert with Liquid Glass to maintain that crucial separation between the UI and content layers and ensure legibility, especially with dynamically scrolling content… As content begins to scroll underneath a glass element, **the effect gently dissolves the content into the background, lifting the glass visually above the moving content**, and allowing floating elements like titles to always remain clear.»

**FACT — два стиля (WWDC25-356):**
- **Soft** — «subtle transition and works well for interactive elements, like buttons or inputs, using Liquid Glass». **По умолчанию**, особенно iOS/iPadOS.
- **Hard** — «mostly used on macOS. It creates a **stronger, more opaque boundary** — ideal for interactive text, controls without backgrounds, or pinned table headers».

**FACT — ограничения (WWDC25-356):** «Scroll edge effects are not decorative. They don’t block or darken like overlays. They simply clarify where UI and content meet, and **shouldn’t be used where there aren’t any floating UI elements**.» + «You should avoid mixing or stacking them» + «**Apply one scroll edge effect per view**. In Split View layouts… each pane can have its own— just keep them consistent in height to maintain alignment.»

**FACT — адаптивность (WWDC25-219):** «When darker content scrolls under, triggering the glass itself to transition to its dark style, the effect intelligently switches to apply a subtle dimming instead, again ensuring contrast and legibility. In some cases, like when there are pinned accessory views under a toolbar… we use a "hard style" effect instead.»

**API (Adopting Liquid Glass):** `scrollEdgeEffectStyle(_:for:)`, `safeAreaBar(edge:alignment:spacing:content:)`, `UIScrollEdgeElementContainerInteraction`.

**INFERENCE для Web:** scroll edge effect = **маска/градиент на верхней/нижней кромке скролл-контейнера**, растворяющий контент в фон (`mask-image` с вертикальным градиентом или `::before` с фоном приложения). Это **не** blur и **не** затемнение — именно поэтому в проекте это дешёвый и безопасный приём.

**FACT (Adopting Liquid Glass) — важное предупреждение:** «If your app has any extra backgrounds or darkening effects behind the bar items, **make sure to remove them, as they will interfere with the effect**.»

### 9.2 Background extension effect (FACT)

«A background extension effect creates a sense of extending a background under a sidebar or inspector, **without actually scrolling or placing content under it**. A background extension effect **mirrors the adjacent content** to give the impression of stretching it under the sidebar, and **applies a blur** to maintain legibility.» API: `backgroundExtensionEffect()`, `UIBackgroundExtensionView`, `NSBackgroundExtensionView`.

**FACT (WWDC25-356):** «sidebars are now **inset** and built with Liquid Glass, allowing content to flow behind them… **Scroll views now extend beneath the sidebar by default**».

**INFERENCE:** «зеркалирование» в Web реализуемо через второй экземпляр фонового изображения/градиента с `transform: scaleX(-1)` и размытием. Для VoidLauncher применимо, только если у sidebar появится «красивый» фон (например, обложка запущенной игры) — иначе зеркалить нечего, и приём даст пустой визуал.

---

## 10. Что Apple подтверждает, что наблюдается, что не раскрыто

| Поведение | Apple подтверждает (FACT) | Наблюдается (OBSERVATION) | Механизм раскрыт | Воспроизводится в Web |
|---|---|---|---|---|
| Название/назначение: material для controls и navigation | Да (HIG, WWDC, API) | Да | Принципы — да | — |
| Lensing: bend/shape/concentrate light в реальном времени | **Да, дословно** (WWDC25-219) | Да | Нет (shader закрыт) | Частично: WebGL-дисплacement |
| Refraction как отдельный эффект | Да (термин «refraction effects») | Да | Нет | Частично |
| Backdrop sampling с области больше элемента | Да (дословно) | Да | Нет | **Нет** в CSS; приблизительно в WebGL |
| Glass не может семплить glass | Да (дословно) | Да | Нет | Аналог — не делать вложенный backdrop-filter |
| `GlassEffectContainer`: shared region + рендер вместе | Да (API) | Да | Нет | **Частично и практически точно** (один слой на группу) |
| Морфинг форм, `glassEffectID`/`Union`/`Transition` | Да (API) | Да | Нет | Частично (CSS/SVG path morph) |
| Адаптация слоёв к фону (тень, тон, dynamic range) | Да (дословно) | Да | Нет | Грубо: CSS/JS по яркости |
| Независимое переключение light/dark | Да | Да | Нет | Грубо: JS/медиа-тема |
| Размер-зависимое переключение light/dark (малые — да, крупные — нет) | Да (дословно) | Да | Нет | **Да**, простым правилом по размеру |
| Размер-зависимая «толщина» материала | Да (дословно) | Да | Нет | Да, параметризацией эффекта |
| Tint, зависящий от яркости подложки (hue/brightness/saturation) | Да (дословно) | Да | Нет | Грубо (`color-mix`) |
| Тень темнее над текстом, светлее над сплошным фоном | Да (дословно) | Да | Нет | Да (JS-подобная логика) |
| Illumination from within по курсору | Да | Да | Нет | **Да**, радиальный градиент |
| Scale/bounce/shimmer | Да | Да | Нет | Да (CSS transform) |
| Materialize через модуляцию преломления, не fade | Да (дословно) | Да | Нет | Да (анимировать blur/tint, не opacity) |
| Recede при потере фокуса окна | Да | Да | Нет | **Да** (Tauri onFocusChanged) |
| Scroll edge effects soft/hard | Да | Да | Нет | Да (маска/градиент) |
| Concentricity (r_inner = r_outer − padding) | Да | Да | Формула — да | **Да**, вручную (CSS-переменные) |
| Варианты Regular/Clear, не смешивать | Да | Да | Нет | Не применимо (используем Regular) |
| Запрет glass-on-glass | Да (дословно) | Да | Нет | Правило проектирования |
| **Конкретный шейдер / формула преломления** | **Нет** | — | **Нет** | — |
| **Алгоритм blur** | **Нет** | Предположительно Gaussian-подобный | **Нет** | Использовать браузерный |
| **Pipeline Metal / порядок compositing** | **Нет** | — | **Нет** | — |
| **Амплитуда/профиль falloff** | **Нет** | — | **Нет** | Подбирать эмпирически |
| **Модель освещения (источники света)** | **Нет** | Частично (свет реагирует на геометрию) | **Нет** | Статическая аппроксимация |

**Явно не раскрытое (INFERENCE-уровень, не утверждать как факт):** шейдеры, формулы преломления, алгоритм blur, последовательность compositing, профиль falloff, модель источников света, поведение на границах между соседними элементами в пикселях. Любые утверждения об этом — гипотеза.

---

## 11. Web: техники рендеринга

### 11.1 CSS

| Приём | Для чего | Качество приближения | Стоимость | Вердикт |
|---|---|---|---|---|
| `backdrop-filter: blur(Npx)` | Blur + цвет | Хорошее | Средняя | **Обязателен (Tier 1)** |
| `backdrop-filter: url(#svgFilter)` | Тот же SVG-фильтр на backdrop | Ограниченное | Средняя–высокая | Осторожно |
| `backdrop-filter: drop-shadow()` | Отделение от фона | Хорошее | Средняя | Полезно, но тень в backdrop = дорого |
| `background: color-mix(...)` / `rgba` | Tint | Грубое | Низкая | Да |
| Градиенты (`linear`/`radial`/`conic`) | Rim, specular, inner glow, ambient spill | Хорошее | **Низкая** | **Обязательно** |
| `mask` / `mask-image` | Форма, soft falloff, scroll edge | Хорошее | Низкая–средняя | Да |
| `clip-path` | Форма | Хорошее | Низкая | Да (но **создаёт backdrop root**!) |
| `border-radius` (в т.ч. наследуемый) | Геометрия, concentricity | Хорошее | **Низкая** | **Обязательно** |
| `box-shadow` (в т.ч. inset) | Адаптивная тень, inner edge | Хорошее | **Низкая** | Да |
| `::before`/`::after` | Rim, specular, glow | Хорошее | **Низкая** | **Обязательно** (2–3 слоя максимум) |
| `mix-blend-mode: screen/overlay` | Аддитивность блика | Хорошее | Низкая | Да (но **создаёт backdrop root**) |
| `transition`/`animation` на `transform`, `filter`, `border-radius` | Динамика | Хорошее | Низкая–средняя | Да |
| `will-change` | Подсказка композитору | — | **Низкая** | Осторожно (см. backdrop root) |
| `contain: paint/layout` | Изоляция | — | **Низкая** | Да |
| `@property` | Типизированные custom properties (анимируемые градиенты) | Хорошее | Низкая | Да |
| `prefers-reduced-motion` / `prefers-reduced-transparency` | Accessibility | — | Низкая | **Обязательно** |
| `@supports` / `feature query` | Fallback tiers | — | Низкая | **Обязательно** |
| `filter: blur()` на самом элементе | Не подходит для glass | — | — | Нет (это не backdrop) |

**FACT (MDN, поддержка):** `backdrop-filter` — **Baseline 2024** (доступно с сентября 2024 в актуальных браузерах). Спецификация: Filter Effects Module Level 2. Поддерживает комбинации: `url("f.svg#filter") blur(4px) saturate(150%)`.

**FACT (MDN, `will-change`):** `will-change` на `opacity`/`filter`/`mask`/`clip-path`/`backdrop-filter`/`mix-blend-mode` **сам создаёт backdrop root** для потомков.

### 11.2 SVG Filters

| Primitive | Назначение для LG | Полезность | Ограничения |
|---|---|---|---|
| `feGaussianBlur` | Blur | Средняя (для blur-content) | Не заменяет `backdrop-filter` (тот фильтрует **позади**) |
| `feDisplacementMap` | **Distortion/refraction** | **Потенциально высокая** | Применяется к отфильтровываемому содержимому, не к «экрану»; нужен явный источник; **дорог при анимации и на больших областях** |
| `feTurbulence` | Микротекстура | Низкая | Риск «грязи», съедает производительность; Apple не упоминает шум |
| `feColorMatrix` | Тон/насыщенность/альфа | Средняя | Матричная, неудобно для адаптации к яркости |
| `feComposite` / `feBlend` | Компоузинг слоёв маски | Средняя | Усложняет пайплайн |
| `feMorphology` / `feConvolveMatrix` | Edge treatment | Низкая | Избыточно для CSS-задач |
| `feOffset` + `feGaussianBlur` + `feComposite` | Drop shadow | Средняя | CSS `box-shadow` дешевле |

**FACT:** `backdrop-filter: url(...)` применяет SVG-фильтр к backdrop — это делает SVG-фильтр применимым к фону (FACT: MDN, синтаксис и пример).

**INFERENCE:** комбинация `backdrop-filter: url(#lg)` с `feTurbulence`+`feDisplacementMap` внутри SVG — теоретически единственный «чисто-CSS-путь» к distortion фона. **Но:** (а) требует статичного источника внутри SVG-фильтра, (б) не даёт screen-space семплинга реального окна, (в) анимация — дорого. Практический вердикт: **не рекомендовать для VoidLauncher**; использовать только статические SVG-эффекты (шейп, паттерн) без анимации.

### 11.3 Canvas / WebGL / WebGPU

| Подход | Screen-space distortion | Backdrop sampling | Производительность | Сложность | Вердикт для VoidLauncher |
|---|---|---|---|---|---|
| Canvas 2D | Ручной pixel lookup | `getImageData` (дорогой readback) | **Низкая–средняя** | Средняя | **Нет** (readback + CPU) |
| **WebGL 1/2** | **Да** (fragment shader, UV offset по нормали) | Да (FBO региона) | **Хорошая** (GPU) | Высокая | **Да, Tier 3** |
| WebGPU | Да | Да | Отличная | Очень высокая | Опционально, не полагаться |

**Когда CSS перестаёт хватать (INFERENCE — критерии перехода на WebGL):**
1. Требуется **видимое искажение** содержимого под материалом (Lensing) — CSS не может.
2. Нужна **амплитуда, зависящая от размера элемента** (толщина материала) с визуально ощутимой разницей.
3. Нужна **адаптация к яркости подложки** в реальном времени на каждом кадре без JS-раундтрипа.
4. Нужно **морфинг формы + материала** одновременно (не только формы).

**Когда CSS достаточно (INFERENCE):**
1. Blur + tint + rim + glow + interaction + recede.
2. Материал на непрозрачном/однотонном фоне.
3. Элементы небольшие, их мало.
4. Нужна широкая совместимость и предсказуемая производительность.

**Цена WebGL (INFERENCE, без выдуманных цифр):** нужен отдельный canvas-слой с FBO, ручная реализация blur (в WebGL нет встроенного гауссова blur для произвольного радиуса — обычно 2–3 прохода с понижением разрешения), ручное управление областью семплинга, корректная работа с `devicePixelRatio`, синхронизация с DOM-геометрией (resize/scroll → recompute), обработка отсутствия WebGL, плюс **дополнительная текстура-память**. Это **отдельный рендер-пайплайн в приложении, которое сейчас рендерится только DOM**.

### 11.4 Backdrop root как практическое ограничение компоновки (FACT + INFERENCE)

Комбинация, которую легко построить неудачно: «родитель с `opacity: .98` + ребёнок с `backdrop-filter`» → эффект у ребёнка семплит **только** пиксели между родителем и ребёнком, то есть **визуально пустоту** → blur не видно.

**RECOMMENDATION (инженерное правило для дизайна компонентов):**
- Не использовать `opacity < 1` на обёртках glass-элементов.
- Если нужен fade — анимировать `backdrop-filter`/маску, а не `opacity` предка.
- Если нужен `clip-path`/`mask` на родителе — glass должен быть на **том же** или **вложенном** уровне, с пониманием, что он увидит только свой слой.
- Проверять цепочку предков в `getComputedStyle` при отладке.

---

## 12. Производительность (критично)

### 12.1 Что дорого, по категориям

| Операция | Механизм нагрузки | Уровень риска (INFERENCE) | Рекомендация |
|---|---|---|---|
| `backdrop-filter: blur()` на большой области | Чтение + размытие back-buffer каждый кадр | **Высокий** | Только для панелей, не для полноэкранных слоёв |
| Большой blur radius | Радиус ядра свёртки (в Chromium — трёхпроходная box-подобная схема) | **Высокий** | 6–14 px; избегать >20 px |
| **N перекрывающихся независимых `backdrop-filter`** | Каждый читает back-buffer **после** предыдущих слоёв → N полных проходов | **Очень высокий** | **Главный антипаттерн.** Один слой на контейнер |
| `backdrop-filter` + анимация `transform` | Пересчёт бэкдропа каждый кадр | Высокий | Анимировать только `transform` у **готового** слоя, не сам blur |
| Анимация самого `blur()` | Пересчёт ядра каждый кадр | **Очень высокий** | Не анимировать blur в цикле; анимировать opacity готового слоя |
| `mix-blend-mode` | Потеря кэширования слоя | Средний–высокий | Локально |
| `clip-path`/`mask` | Пересчёт маски | Средний | Статические маски |
| SVG `feDisplacementMap` | Пиксельный доступ к карте на каждый пиксель | **Очень высокий** | Только статически, не на анимируемом контенте |
| SVG `feTurbulence` | Генерация шума (дорого) | Высокий | Избегать |
| `filter: blur()` на большом DOM-элементе | Отдельный буфер на элемент | Высокий | Не для контентных панелей |
| WebGL fullscreen-пасс | Заполнение GPU-памяти, bandwidth | **Очень высокий** | Только при доказанной необходимости |
| Много мелких glass-элементов | Много слоёв композиции | Высокий | Объединять в один контейнер |
| `will-change` без нужды | Постоянная память под слой | Средний | Ставить точечно, снимать |
| Scroll под glass | Backdrop меняется каждый кадр | **Высокий** | Маска (scroll edge) дешевле blur; ограничивать область |

**INFERENCE — порядок стоимости (от дорогого к дешёвому):**
1. Много независимых перекрывающихся `backdrop-filter` (главный bottleneck).
2. Анимируемые фильтры (blur/SVG) в цикле.
3. WebGL-пасс на большой области.
4. Большой статический blur на большой области.
5. Статический малый blur на небольших панелях.
6. Градиенты, маски, тени, blend (дешёвые, но создают слои).

### 12.2 Правила и бюджеты (RECOMMENDATION, без выдуманных FPS)

- **Один `backdrop-filter`-слой на группу** — не на элемент. Это прямо следует из `GlassEffectContainer` (FACT) и является главным рычагом.
- **Бюджет площади:** суммарная площадь blur-областей — **меньшинство** окна. Sidebar + titlebar + одна модалка — нормально; «blur на всю страницу» — нет.
- **Бюджет количества:** одновременно активных blur-слоёв — единицы, не десятки.
- **Радиус:** 6–14 px для панелей; меньше для мелких контролов; больше только для крупных модалей.
- **Не анимировать blur.** Появление/исчезновение — через `opacity` **готового** слоя или через маску.
- **Кэшировать:** статические rim/градиенты компилируются один раз; не пересоздавать градиенты в React на каждом рендере (вынести в CSS-переменные/статические правила).
- **Изоляция:** `contain: paint` на скролл-контейнеры под glass; избегать layout-thrashing (не читать `getBoundingClientRect` в цикле рендера — использовать `ResizeObserver`).
- **Упрощение при слабом GPU:** `@supports` + ручной quality-флаг → Tier 1.
- **Профилировать обязательно.** Без измерений (DevTools Performance, WebView2 → `edge://gpu`, Tauri devtools) любые утверждения о «дёшево/дорого» остаются гипотезой. **В этом документе намеренно нет конкретных FPS/процентов** — их нельзя выдумывать.

---

## 13. Доступность и читаемость

### 13.1 FACT — системные модификаторы (WWDC25-219)

«These act as modifiers on the material that change certain layers of Liquid Glass, without sacrificing its magic.
- **Reduced Transparency**, makes Liquid Glass **frostier and obscures more of the content behind it**.
- **Increased contrast**, makes elements **predominantly black or white and highlights them with a contrasting border**.
- **Reduced Motion**, **decreases the intensity of some effects and disables any elastic properties** for the material.
These are available **automatically** whenever you use the new material.»

**FACT (Adopting Liquid Glass):** «people can choose a **preferred look for Liquid Glass** in their device’s settings, or turn on accessibility settings that reduce transparency or motion… These settings can **remove or modify certain effects**… Ensure you test your app’s custom elements, colors, and animations with different configurations of these settings.»

### 13.2 Правила читаемости (FACT + INFERENCE)

| Правило | Источник | Следствие для VoidLauncher |
|---|---|---|
| Материал подчинён контенту: «letting as much of the content through as possible» | FACT (WWDC25-219) | Не перекрывать контент плотным blur'ом |
| Символы/глифы зеркалят полярность фона | FACT | Проверять контраст иконок на светлом и тёмном фоне |
| Маленькие элементы переключают light/dark, большие — нет | FACT | Sidebar не «инвертируется» |
| Clear требует затемняющего слоя, иначе legibility заметно хуже | FACT | Не использовать «прозрачный» вариант в desktop |
| Tint — только для акцентов, не для всех | FACT | Не тонировать всё подряд |
| Стили скролл-эффекта: soft — по умолчанию, hard — для интерактивного текста/контролов без фона | FACT (WWDC25-356) | В плотном desktop-UI уместнее `hard` |
| Избегать пересечений контента и glass в статичном состоянии | FACT (WWDC25-219) | Контент не должен попадать под материал в покое |

**RECOMMENDATION — что нельзя делать:**
- Делать blur настолько сильным, чтобы под ним нельзя было прочитать текст.
- Размещать мелкий текст (логи, версии, размеры) на сильном tint без проверки контраста.
- Анимировать материал непрерывно (постоянный shimmer) — вредно и для WCAG.
- Полагаться **только** на `prefers-reduced-transparency` (в проекте **уже есть** `prefers-reduced-motion`-блок, но **нет** `prefers-reduced-transparency` — это нужно добавить; **почему одного медиа-запроса недостаточно** — см. §13.3).
- Терять `:focus-visible` — glass не должен ломать фокусные кольца.

### 13.3 `prefers-reduced-transparency` на целевой платформе (FACT + INFERENCE)

Проверено по официальным данным (S13, S14) — **не** по предположению:

| Браузер | Поддержка | С версии |
|---|---|---|
| Chrome / Edge / Chrome Android | **да** | **119** (Chrome — 31.10.2023, Edge — 02.11.2023) |
| Firefox / Safari | **нет** (WPT stable = 0) | — |

- **FACT:** фича **не** является Baseline — MDN помечает её «Limited availability» и «Experimental», потому что она не работает в Firefox и Safari (S13).
- **FACT:** Chromium/Edge поддерживают её с 119 (S14) ⇒ **в WebView2 фича работает**, так как evergreen-runtime WebView2 следует за Edge. Для VoidLauncher (Windows-only) рекомендация «добавить `prefers-reduced-transparency`» рабочая, а не утопичная.
- **FACT:** системная настройка в Windows 10/11 — «Settings → Personalization → Colors → Transparency effects» (S13). В Windows 11 она **включена по умолчанию**, поэтому `reduce` срабатывает лишь у тех, кто выключил её явно.
- **FACT (S14):** daily usage в Chrome — **≈ 0,05 %**.
- **FACT:** спецификация — Media Queries Level 5, документ ещё в статусе черновика (S13).
- **OBSERVATION:** Apple ведёт в standards-positions возражение по **privacy** (S14) — сигнал, что фича может измениться; строить на ней единственную гарантию нельзя.

**INFERENCE:** `prefers-reduced-transparency` — «значимая, но недостаточная» мера. Опора только на неё означает accessibility-фичу, которая у подавляющего большинства пользователей **молча не сработает**: системную настройки по умолчанию почти никто не выключает.

**RECOMMENDATION — двухуровневая схема:**
1. **Уровень 1 (основной):** собственный переключатель в настройках лаунчера («Упростить прозрачность»), сохраняемый в localStorage. Он переключает **один набор токенов** `--lg-alpha-*`, `--lg-blur-*`, `--lg-refraction-*`, `--lg-tint-alpha`. Такой путь работает у всех и не зависит ни от браузера, ни от OS.
2. **Уровень 2 (уважение к системе):** `@media (prefers-reduced-transparency: reduce)` переключает те же токены, **но только если** пользователь не задал явный выбор на Уровне 1. Явная настройка лаунчера должна **побеждать** системную, иначе она молча перезаписывается при каждом запуске.
3. **Не** пытаться детектировать настройку из JS: в WebView2 нет надёжного API для её чтения, доступен только медиа-запрос (INFERENCE — из отсутствия такого API в S11/S13 и архитектуры WebView2).

---

## 14. Что НЕ является Liquid Glass

| Ошибка | Почему это не LG (с опорой на FACT) | Что делать вместо |
|---|---|---|
| Только `background: rgba(...)` | Нет семплинга, нет преломления, нет адаптации | Минимум `backdrop-filter` |
| Только `backdrop-filter: blur()` | Нет lensing, нет edge/specular, нет адаптации к фону, нет общего контейнера | Добавить rim, tint, interaction, контейнер |
| Frosted/matte look вместо оптического | Apple: рассеивание **заменено** преломлением | Повысить edge-контраст, добавить specular |
| Белая жёсткая граница `border: 1px solid #fff` | Apple строит материал из слоёв; жёсткая рамка — не оптическая граница | Мягкий inner edge (inset shadow/градиент) |
| Сильная чёрная тень | Тень у LG **адаптивна** и деликатна | Малая тень, зависящая от подложки |
| Декоративный градиент поверх blur | Разрушает адаптацию; tint должен быть привязан к яркости подложки | Адаптивный tint либо очень слабый статический |
| Чрезмерная прозрачность | «leaving tools at risk of not being discernible» | Плотнее там, где мелкий текст |
| Одинаковый материал на всех элементах | «Liquid Glass is most effective when applied judiciously… avoid using it indiscriminately» | Иерархия: акцент только на функционально важном |
| **Glass-on-glass** | Прямо запрещён (FACT) | Верхний слой — fill/vibrancy |
| Стекло в content layer | «muddy the hierarchy» | Материал только в navigation/functional layer |
| Одинаковая интенсивность независимо от фона | Каждый слой адаптируется | Считать яркость подложки |
| Стекло поверх текста в статичном состоянии | «avoid intersections between content and Liquid Glass» | Отодвинуть/отмасштабировать контент |
| Fade-in/out через `opacity` | «materialize… by gradually modulating the light bending and lensing» | Анимировать силу материала |
| Одинаковый материал для мелкого и крупного | Размер меняет «толщину» и полярность | Параметризовать по размеру |
| Смешение Regular и Clear | «They should never be mixed» | Один вариант (Regular-подобный) |
| Много стеклянных слоёв | «avoid overcrowding or layering Liquid Glass elements on top of each other» | Объединять, не наслаивать |
| Стекло ради декоративности | «Liquid Glass aims to bring the user's attention to the content beneath it» | Только функциональные слои |
| Разные радиусы у вложенных элементов (non-concentric) | «corners that feel too pinched — or flared… break the sense of balance» | Concentricity |

---

## 15. Применение к VoidLauncher: анализ существующего интерфейса

> Только анализ. Никаких изменений в UI.

### 15.1 Фактический стек (проверено в репозитории)

- Tauri 2, React 19, TypeScript 5.8, Vite 7, Zustand 5, `lucide-react`
- Windows-only; WebView = **WebView2 (Edge/Chromium)**
- CSP задан в `tauri.conf.json` (`default-src 'self'`, `img-src`, `connect-src 'self'`, `style-src 'self' 'unsafe-inline'`, `font-src 'self'`)
- `src/index.css` — **3306 строк**, дизайн-токены на CSS-переменных (без CSS-модулей / CSS-in-JS)
- Окно: 1200×800, min 900×600, `decorations: false` (свой titlebar)

### 15.2 Что уже есть (важно для планирования)

| Находка | Факт | Значение |
|---|---|---|
| Токены `--surface-glass`, `--surface-glass-hover`, `--surface-glass-active` | Уже существуют | Именованная база для glass-вариации |
| **Все 4 темы — тёмные** | `Theme = 'standard' \| 'dark' \| 'blueprint' \| 'ember'`; `colorScheme = 'dark'` для всех | **Критично** — см. 15.5 |
| В темах `blueprint` / `ember` `--surface-glass` — **непрозрачный hex** (`#1A1D1F`, `#1A1816`) | `index.css` | Под ними нечего преломлять/размывать |
| `backdrop-filter: none` | Есть только в `.modal-overlay--bare` (намеренный bypass) | Хорошо: значит подход осознанный |
| `prefers-reduced-motion: reduce` блок | Уже есть (отключает `.stagger-in`, `.skeleton`, `.icon-shine::after`, `.progress-fill::after`, замедляет спиннер) | Фундамент для accessibility готов |
| `prefers-reduced-transparency` | **Отсутствует** | Добавить **+ собственный переключатель** (§13.3) |
| `@property`, `mix-blend-mode` | **Не используются** | Нет заготовок |
| Шкала радиусов | `--radius-sm/md/lg/xl` (6/10/14/20 в standard; 6/8/10/12 в blueprint) + `--radius-full` | Готова для concentricity |
| Тайминги | `--transition-fast: 150ms`, `--transition-normal: 250ms`, `cubic-bezier(0.4, 0, 0.2, 1)` | Уже соответствует правилам проекта |
| Layout-метрики | `--titlebar-height: 40px`, `--sidebar-width: 72px`, `--sidebar-expanded` | Опорные значения |
| `will-change: transform, width` | На sidebar (анимируется ширина) | `transform`/`width` **не** создают backdrop root — безопасно |
| Скролл-контейнеров | **8** (пересчёт по `index.css`: `.main-content`, `.custom-select__list`, `.modal`, `.modpacks-overlay__body`, `.version-list`, `.console`, `.md-content pre`, `.game-logs-picker-panel`) | Каждый под материалом = backdrop пересчитывается при скролле |
| ↑ уточнение после реализации | `.main-content` **исключён** из материалов намеренно: блюр на всю область контента поглощал бы эффект обоев и оставлял бы стеклянным элементам уже размытый фон. Итоговое разделение: **13 стеклянных поверхностей** (`.titlebar`, `.sidebar`, `.modal`, `.custom-select__list`, `.ctx-menu`, `.toast`, `.tooltip-bubble`, `.instance-card`, `.modpack-card`, `.modpack-detail-panel`, `.modpacks-overlay__bar`, `.selection-bar`, `.game-logs-picker-panel`), колодцы без блюра (`.settings-section`, мелкие контролы, `.version-list`), остальное — без материала | Пустое место = сырые обои; блюр только на элементах из стекла |
| Z-index слои | titlebar 100, sidebar 50, `.modal-overlay` 1000, `.modpacks-overlay` 1400 | Уже выстроенная иерархия |

### 15.3 Поверхности-кандидаты

| Элемент | Текущее состояние | Оценка | Обоснование |
|---|---|---|---|
| `.titlebar` (40 px) | `position: relative`, `background: var(--bg-secondary)`, `border-bottom`, `z-index: 100`, `-webkit-app-region: drag` | **Высокий** | Тонкая полоса; material по всей длине = правильная «navigation layer». Но: `drag`-регион не должен ломать hit-testing |
| `.sidebar` (72 px → expanded) | `position: relative` (в flex-row), `background: var(--bg-secondary)`, `border-right`, `z-index: 50` | **Высокий** | Классический navigation layer. Фактически «floating» (вогнутый угол) отсутствует — потребует либо inset-оформления, либо стекла |
| `.modal` | `background: var(--bg-secondary)`, `border: 1px solid var(--surface-border)`, `--radius-xl`, `box-shadow: var(--shadow-xl)`, `slideUp` | **Высокий** | Эквивалент sheet; но `.modal-overlay` = `rgba(0,0,0,0.7)` → **glass-on-glass** со scrim, см. 15.5 |
| `.modpacks-overlay` | `position: fixed`, `z-index: 1400`, `top: var(--titlebar-height)`, `background: var(--bg-primary)` — **непрозрачный** | **Средний** | Полноэкранный слой — риск производительности; но при `bg-primary` + blur может работать как «единый» материал |
| `.log-toolbar` (`LogToolbar.tsx`) | Компактный | **Высокий** | Идеальный размер для «тонкого стекла» |
| `GameRunningBadge` | Floating | **Средний–высокий** | Маленький capsule; конфликт с `focusStore` (freeze UI при запуске игры) |
| `InstanceList` / списки | Плотные списки со скроллом | **Средний (осторожно)** | Скролл под blur = пересчёт; контент-слой по Apple — **не** стекло |
| `InstallOverlay.tsx` | Progress | **Низкий** | Критичный прогресс; лучше без стекла |
| `MicrosoftLoginCard` | `--bare` overlay (без scrim) | **Низкий** | Намеренно «чистая» карточка; glass нарушит совпадение со страницей Login |

### 15.4 Где glass улучшит иерархию, а где помешает

**Улучшит (RECOMMENDATION):**
- **Разделение navigation layer и content layer** — сейчас sidebar и content оба на непрозрачном `--bg-secondary`/`--bg-primary`, граница только `--surface-border` (1 px). Стекло даст **оптическое** разделение вместо линии.
- **Выразительность scroll edge** — при 8 скролл-контейнерах маска на кромке резко повысит читаемость.
- **Фокус на модалках** — `.modal-overlay--bare` (без scrim) сейчас единственное место без затемнения; стекло там даст фокус без потери контекста. Но нужен «bare-glass» вариант, а не `.modal` со scrim.
- **Иерархию мелких элементов** — сейчас нет визуальной разницы между «просто фон» и «активная кнопка»; материал + tint это решает.

**Помешает (RECOMMENDATION):**
- **Скролл-контейнеры под материалом** — 8 скролл-контейнеров = потенциальный постоянный пересчёт бэкдропа. Если делать — только blur без анимации и без `will-change: backdrop-filter`.
- **Логи** (`Logs.tsx`, `GameLogs.tsx`) — моноширинный мелкий текст, высокая плотность; контраст критичен, материал опасен.
- **`.modpacks-overlay` во весь экран** — самый дорогой возможный слой при минимальном выигрыше.

### 15.5 Критические блокеры и конфликты с принципами LG

| # | Конфликт | Суть | Рекомендация |
|---|---|---|---|
| 1 | **Тёмные темы только** | Apple: «Unlike previous materials that had a **fixed light or dark appearance**, each layer continuously adapts» и «can also **independently switch between light and dark**». Фиксированная тёмная тема убивает **центральную** адаптацию LG | Реализовать адаптацию **относительно** яркости подложки (не абсолютную light/dark): тёмный фон + светлый лог под панелью → панель светлеет. Это честная имитация принципа, а не смена темы |
| 2 | **`blueprint` / `ember` — непрозрачные поверхности** | Под непрозрачным `--surface-glass` нет бэкдропа → blur/refraction не дают эффекта | Либо принять, что эти темы получают «плоский» материал (fallback), либо сделать поверхности полупрозрачными |
| 3 | **Glass-on-scrim-glass** | `.modal-overlay` = `rgba(0,0,0,0.7)`; `.sidebar` под ним. Если и sidebar, и `.modal` станут стеклом — прямой запрет | Материал применять к **одному** слою; верхний — fill |
| 4 | **Backdrop root риск** | `opacity < 1`, `mask`, `clip-path`, `mix-blend-mode` у предков убивают эффект у потомков | Проверять цепочку; не оборачивать glass в `opacity` |
| 5 | **8 скролл-контейнеров** | Backdrop пересчитывается на каждом кадре скролла | Scroll edge **маской** (дёшево) вместо blur под контентом |
| 6 | **`prefers-reduced-transparency` отсутствует** | Accessibility-требование Apple | Добавить **+ свой переключатель** (§13.3) |
| 7 | **`decorations: false` + `-webkit-app-region: drag`** | Стеклянный titlebar с drag-регионом: hit-testing, resize-области | Проверять, что drag-регион не конфликтует с pointer-событиями материала (glow от курсора) |
| 8 | **Нет `@property` / `mix-blend-mode`** | Нет заготовок для анимируемых градиентов и аддитивных бликов | Добавить точечно при реализации |

### 15.6 Динамический фон: нужен ли

**FACT (WWDC25-219):** «On larger elements, like sidebars, the appearance of Liquid Glass is informed by **the ambient environment within the app**. Light from colorful content nearby can subtly spill onto its surface.» и «Light from colorful content nearby can **subtly spill** onto its surface, reinforcing the material's context».

**INFERENCE для VoidLauncher:** «окружающая среда» в лаунчере бедна — нет фотообоев, нет media-rich контента под sidebar. Ближайший аналог «цветного окружения» — **обложка/баннер запущенной сборки** (`--banner-error-bg`, `bannerPresets.ts`, `Instance.banner`) и цветовые акценты (`--accent`). Это даёт повод для **лёгкого цветового spill** на крупных элементах, но **не** оправдывает сильные эффекты.

### 15.7 Где эффект будет особенно заметен, а где незаметен

**Очень заметен (RECOMMENDATION):**
- Над **логами** (`GameLogs.tsx`) — высокий контраст, цветные уровни `ERROR/WARN/INFO` → блюр + адаптация тонa дают выраженный эффект и одновременно улучшают читаемость. Но осторожно с мелким текстом.
- Над **списком сборок с баннерами** — цветные баннеры = «media-rich content» (единственное место, где применим даже аналог «Clear»).
- В **`.modal`** (когда scrim убрать) — над размытым контентом.
- **Плавающий `GameRunningBadge`** над пёстрым логом.

**Практически незаметен / бессмысленен (RECOMMENDATION):**
- Над **однотонным** `--bg-primary` — нечего преломлять; blur даст лишь мутность.
- **Внутри** `content layer` (списки, таблицы) — Apple прямо отговаривает.
- На **мелких** элементах (< ~24 px) — нет места для краёв и градиентов.
- В **темах blueprint/ember** с непрозрачными поверхностями (см. 15.5 #2).

---

## 16. Предлагаемая архитектура материала

Слои выведены из: FACT (Apple описывает highlights/shadow/lensing/tint как отдельные адаптирующие слои + двухслойная модель `glassEffect`) + INFERENCE (порядок рендеринга, аналогичный любому physically-based стеклу).

| # | Слой | Назначение | Вход | Выход | Web-технология | Стоимость | Обязателен | Упрощение |
|---|---|---|---|---|---|---|---|---|
| 0 | **Geometry** | Форма, радиусы, concentricity | Layout, размер | CSS shape | `border-radius`, `clip-path`, CSS-переменные | **Низкая** | **Да** | Только rect |
| 1 | **Thickness parameter** | «Толщина» по размеру элемента | Габариты | скаляр 0–1 | CSS-класс/переменная + `matchMedia`/ResizeObserver | **Низкая** | **Да** | 2 уровня (мелкий/крупный) |
| 2 | **Backdrop sampling** | Получить то, что позади | Backdrop root | Сэмпл | `backdrop-filter` (неявно) | Средняя | **Да** | — |
| 3 | **Lensing / distortion** | Преломление, утолщение у кромок | Сэмпл + нормали формы | Искажённый сэмпл | **WebGL-шейдер** (Tier 3); CSS/SVG — нет | Очень высокая | **Нет** | **Пропустить** |
| 4 | **Blur + scatter** | Размытие, «толщина» рассеяния | Сэмпл | Размытый | `backdrop-filter: blur(Npx)`, N от слоя 1 | Средняя | **Да** | 6–10 px |
| 5 | **Luminance/tint adaptation** | Отклик на яркость и цвет подложки | Размытый + `--accent` | Тон материала | CSS-переменные + (JS или `color-mix`) | Низкая | **Да** | Статический слабый градиент |
| 6 | **Shadow (adaptive)** | Отделение; плотнее над текстом | Яркость подложки | Тень | `box-shadow` (+ чтение яркости) | **Низкая** | **Да** | Одна статическая тень |
| 7 | **Edge / rim / specular** | Силуэт, выпуклость, блик | Форма + направление света | Кромка | `::after` + градиент + `mask` + `mix-blend-mode` | **Низкая** | **Да** | Тонкий градиентный rim |
| 8 | **Inner glow (interaction)** | Отклик на курсор | Pointer coords | Свечение | `::before` + `radial-gradient` на `--mx/--my` | **Низкая** | **Да** (Tier 2) | Только scale |
| 9 | **Foreground vibrancy** | Текст/иконки контрастны к материалу | Тон материала | Цвет контента | CSS-переменная `--fg-on-glass` | **Низкая** | **Да** | Наследовать цвет текста |
| 10 | **Scroll edge** | Разделение UI/контента при скролле | Позиция скролла | Маска кромки | `mask-image` / `::before` градиент | **Низкая** | **Да** | Тонкая маска |
| 11 | **Motion response** | Scale/bounce, materialize | Состояние | Transform/материал | `transition`/`animation` | Низкая–средняя | **Да** (минимально) | Только `transform`+`opacity` |
| 12 | **Focus/attention state** | Recede при потере фокуса окна | Tauri focus event | Приглушение | Класс на `<html>` + CSS | **Низкая** | **Да** (desktop) | — |
| 13 | **Container / shared pass** | Единый слой на группу, слияние форм | Группа элементов | Общий материал | **Один** `backdrop-filter` на родителе | Средняя | **Да** при группах | — |
| 14 | **Accessibility tiers** | Reduced transparency/contrast/motion | Media queries | Изменённый материал | `@media (prefers-*)` | **Низкая** | **Да** | Полное отключение |
| 15 | **Quality tiers** | Fallback по возможностям | `@supports` | Упрощённый материал | `@supports` + флаг | **Низкая** | **Да** | Tier 1 |

**Порядок рендеринга (снизу вверх):** 2 → 3 → 4 → 5 → 6 → 7 → 8 → **9 (контент)** → 10/11/12 (состояния).
Слои 0, 1, 13, 14, 15 — не пиксельные, а управляющие.

**INFERENCE — ключевой вывод модели:** слои **3 (lensing)** и **4 (blur)** — единственные дорогие пиксельные операции. Всё остальное (5–12) — **почти бесплатные** CSS-слои, которые и создают ощущение материала. Отсюда: **максимум качества на единицу стоимости дают слои 5–12, а не слой 3.**

---

## 17. Уровни реализации

### Tier 1 — Минимальный

**Визуально:** полупрозрачная панель с мягким размытием фона, слабый tint, тонкий внутренний край.
**Состав:** слои 0, 1, 2, 4 (6–10 px), 5 (слабый), 6 (одна тень), 7 (тонкий rim).
**Сложность:** низкая. **Стоимость:** низкая–средняя.
**Ограничения:** нет преломления, нет адаптации к яркости, нет интерактива; приближает к «frosted», не к Liquid Glass.
**Когда применять:** fallback для слабого GPU, неподдерживаемые браузеры, настройки доступности, и как **первый** шаг, чтобы проверить визуальную приемлемость на реальных фонах лаунчера.

### Tier 2 — Продвинутый (рекомендуемая цель)

**Визуально:** материал читается как стекло — есть толщина, адаптация к фону, живой отклик на курсор, корректная геометрия, работающая иерархия.
**Состав:** Tier 1 + слой 5 (реальная адаптация по яркости), 6 (адаптивная тень), 8 (glow от курсора), 9 (foreground, зеркалящий фон), 10 (scroll edge), 11 (scale + materialize без fade), 12 (recede при потере фокуса), 13 (общий контейнер), 14, 15.
**Сложность:** средняя. **Стоимость:** средняя (доминируют #2 и #13).
**Ограничения:** преломления нет — при сильном контрасте под панелью «стеклянность» читается слабее, чем в Apple.
**Когда применять:** **основной вариант для VoidLauncher.** Даёт наилучшее соотношение «характер / производительность / риск».

### Tier 3 — Максимально близкий web-вариант

**Визуально:** реальное преломление содержимого, толщина по размеру, настоящая адаптация, морфинг формы и материала.
**Состав:** Tier 2 + слой 3 (WebGL/WebGPU screen-space displacement), возможно полноценный морфинг (SVG-path + общий FBO), specular, зависящий от формы.
**Сложность:** высокая. **Стоимость:** высокая (отдельный рендер-пайплайн, 2–3 blur-прохода, ручное управление областью, синхронизация с DOM, `devicePixelRatio`, отсутствие-WebGL fallback).
**Ограничения:** заметная сложность поддержки; в проекте, рендерящемся только DOM, это вторая рендер-система; на интегрированной графике Windows риск.
**Когда применять:** **только** если визуальное сходство оправдывает цену на **одном** изолированном элементе (например, hero-баннер запущенной сборки или модалка), **не** на всей навигации. Всегда с fallback на Tier 2 и Tier 1.

### Сравнение

| | Tier 1 | Tier 2 | Tier 3 |
|---|---|---|---|
| Blur | ✅ | ✅ | ✅ |
| Адаптация к фону | ❌ | ✅ | ✅ |
| Адаптивная тень | ❌ | ✅ | ✅ |
| Rim / specular | минимально | ✅ | ✅ |
| Преломление (lensing) | ❌ | ❌ | ✅ |
| Интерактив (scale/glow) | ❌ | ✅ | ✅ |
| Scroll edge | ❌ | ✅ | ✅ |
| Recede при потере фокуса | ❌ | ✅ | ✅ |
| Морфинг формы | ❌ | частично | ✅ |
| Accessibility-тиры | ✅ | ✅ | ✅ |
| Стоимость | низкая | средняя | высокая |
| Сложность поддержки | низкая | средняя | высокая |

---

## 18. Принципы для будущей реализации

Обязательные инженерные правила:

1. **Blur ≠ Liquid Glass.** `backdrop-filter: blur()` — один слой из 15. Не выдавать его за результат.
2. **Lensing — центральный, но не обязательный слой.** Если Tier 3 не сделан, компенсировать edge/rim/tint/interaction, а не выдумывать искажение в CSS.
3. **Один blur-слой на группу.** Не дублировать `backdrop-filter` на каждом дочернем элементе. Это главный рычаг производительности и ближайший аналог `GlassEffectContainer`.
4. **Проверять backdrop root.** `opacity < 1`, `filter`, `mask`, `clip-path`, `mix-blend-mode`, `will-change` на этих свойствах у предка — убивают эффект у потомков. Это не баг, а спецификация.
5. **Не использовать `opacity` для появления материала.** Materialize — через модуляцию blur/tint/rim. `opacity` допустим только для **готового** слоя.
6. **Никогда не размывать контент настолько, чтобы он стал нечитаемым.** Читаемость — критерий номер один, выше «красивости».
7. **Материал — только для функционального слоя** (navigation, controls, overlays). Content layer (списки, логи, таблицы) — не стекло.
8. **Запрет glass-on-glass.** Верхний слой над стеклом — fill/vibrancy/тон, а не второе стекло.
9. **Не смешивать Regular и Clear-подобное поведение.** Для VoidLauncher — только Regular (адаптивный). Прозрачный вариант без затемняющего слоя в desktop-UI недопустим.
10. **Размер определяет материал.** Крупный элемент = «толще» (больше blur, глубже тень, сильнее rim). Мелкий = тоньше. Это дешёвый и очень заметный приём.
11. **Не переключать полярность у крупных элементов.** Мелкие могут «инвертироваться» по яркости подложки; sidebar/large panels — нет.
12. **Foreground зеркалит фон.** Иконки/текст на стекле должны контрастировать с фактическим тоном материала.
13. **Tint — только для акцентов.** Не тонировать всё подряд (тогда ничего не выделяется).
14. **Concentricity обязательна.** `r_inner = r_outer − padding`. Иначе «pinched/flared» углы.
15. **Анимировать только `transform`/`opacity` у готового слоя.** Не анимировать `blur()`, `backdrop-filter`, SVG-фильтры в цикле.
16. **`prefers-reduced-transparency` и `prefers-reduced-motion` — обязательны**, но прозрачность снижать нужно **двумя** уровнями (§13.3): собственный переключатель в настройках (основной путь) **+** `prefers-reduced-transparency` (уважение к системе; в реальности срабатывает у ≈ 0,05 % пользователей). `reduced-motion` блок уже есть, новые анимации должны в него попасть.
17. **Градиенты/маски/тени — предпочтительнее пиксельных фильтров.** Они дают 80% ощущения материала при ~0% стоимости.
18. **Fallback-тиры обязательны.** `@supports` + ручной quality-флаг. Никогда не «падать» при отсутствии эффекта.
19. **Никогда не выдавать INFERENCE за FACT.** В коде/документации проекта не утверждать внутренние механизмы Apple как факт.
20. **Не подражать iOS-компонентам.** Переносить **принципы** (слои, концентричность, адаптация, экономия), а не конкретные элементы Apple. Лаунчер — плотный desktop-UI: capsule — точечно, rounded-rect — как основа.
21. **Профилировать, а не угадывать.** Любое утверждение о «дёшево/дорого» подтверждается измерением. Не публиковать выдуманные FPS/проценты.
22. **Сохранять существующие системы проекта.** Токены `--surface-glass*`, шкала `--radius-*`, `--transition-*`, `prefers-reduced-motion` — расширять, не дублировать. Анимации 150–250 ms ease-out (правило проекта).

---

## 19. Сводка: факты против выводов

| Категория | Что относится |
|---|---|
| **FACT** (подтверждено первичным источником) | Название и назначение; Lensing как «bend/shape/concentrate light»; двухслойная модель `glassEffect`; sampling region больше элемента; запрет sampling other glass; `GlassEffectContainer` и shared region; морфинг (`glassEffectID`/`Union`/`Transition`); адаптация слоёв к фону; переключение light/dark; **размер-зависимое** правило (малые переключаются, крупные нет); foreground зеркалит фон; размер-зависимая «толщина»; адаптивная тень; illumination from within; scale/bounce/shimmer; materialize без fade; recede при потере фокуса окна; scroll edge effects soft/hard; background extension effect; concentricity и три типа форм; Regular/Clear и 3 условия для Clear; запрет glass-on-glass; glass не в content layer; адаптивная ожупка; accessibility-модификаторы; performance-предупреждения Apple |
| **OBSERVATION** (наблюдаемо, без раскрытого алгоритма) | Искажение у скруглений; «блеск», зависящий от формы; визуальное отличие края и центра; «живость» на статичном фоне; более выраженный «объём» крупных панелей |
| **INFERENCE** (инженерный вывод) | Screen-space displacement малой амплитуды как web-замена lensing; нормали/SDF формы для преломления; невозможность расширенной sampling region в CSS; параметр «толщины» по размеру; WebGL-критерии перехода; раскладка по стоимости; порядок рендеринга; backdrop root как ловушка; scroll edge через маску |
| **RECOMMENDATION** (для VoidLauncher) | Tier 2 как цель; Tier 1 как fallback; Tier 3 точечно; один blur-слой на группу; не трогать content layer; dark-only → относительная адаптация; снижать прозрачность двумя уровнями — свой переключатель + `prefers-reduced-transparency` (§13.3); проверить backdrop root; параметризация по размеру; scroll edge маской; recede по Tauri focus; непрозрачные темы blueprint/ember = плоский материал |

**Явно не раскрытое Apple (не утверждать):** конкретные шейдеры; формулы преломления; алгоритм blur; последовательность compositing в Metal; профиль falloff у кромок; модель источников света; попиксельное поведение на границах соседних элементов.

---

## 20. Источники

Все источники ниже **фактически получены и прочитаны** при подготовке документа.

### Apple — первичные (design + видео)

| # | Источник | Тип | Что взято |
|---|---|---|---|
| S1 | [Meet Liquid Glass — WWDC25 Session 219](https://developer.apple.com/videos/play/wwdc2025/219/) | **Primary (Apple, полный транскрипт)** | Определение «digital meta-material that dynamically bends and shapes light»; **Lensing** как основной визуальный признак; противопоставление «scattered» vs «bends, shapes, concentrates»; materialize через модуляцию преломления (не fade); четыре источника адаптации; адаптация слоёв; **размер-зависимое переключение light/dark**; foreground зеркалит фон; размер-зависимая «толщина»; highlights/geometry/device motion; **адаптивная тень**; illumination from within; recede при потере фокуса окна; scroll edge effects (soft/hard, адаптивность); **Regular vs Clear + 3 условия**; tint-по-яркости; **запрет glass-on-glass**; glass не в content layer; избегать пересечений контента и glass; **accessibility-модификаторы** |
| S2 | [Build a SwiftUI app with the new design — WWDC25 Session 323](https://developer.apple.com/videos/play/wwdc2025/323/) | **Primary (Apple, полный транскрипт)** | «adaptive material for controls and navigational elements»; navigation zoom transition / morph из кнопки; `toolbar` grouping и `ToolbarSpacer`; `sharedBackgroundVisibility`; scroll edge effect автоматически; `searchToolbarBehavior`; `buttonStyle(.glass/.glassProminent)`; `backgroundExtensionEffect`; `scrollEdgeEffectStyle`; **`glassEffect(_:in:)`** (варианты, форма, tint, `.interactive()`); **`GlassEffectContainer` + `glassEffectID`** (morphing); запрет sampling other glass; **concentricity** |
| S3 | [Get to know the new design system — WWDC25 Session 356](https://developer.apple.com/videos/play/wwdc2025/356/) | **Primary (Apple, полный транскрипт)** | Liquid Glass как **functional layer**; три типа форм (fixed / capsule / concentric, `r = r_outer − padding`); pinched/flared углы; capsule в dense UI; apply material к контролу, не к внутренним вью; **иерархия через layout/grouping вместо декора**; pairing glass + dimming layer при модальности; recede при углублении; **scroll edge effects** (soft по умолчанию, hard для macOS; не использовать без floating UI; один на view; не смешивать); **background extension effect** и его правило «текст и контролы должны быть СВЕРХУ, чтобы избежать визуальных искажений»; continuity / shared anatomy |
| S4 | [Apple Human Interface Guidelines — Materials](https://developer.apple.com/design/human-interface-guidelines/materials) | **Primary (Apple)** | Ссылка подтверждена как канонический раздел HIG по материалам (упоминается как ресурс в S1/S2/S3). **Ограничение:** страница рендерится через JavaScript, автоматическое извлечение текста не удалось; **все конкретные утверждения в документе взяты из S1–S3 и S5–S11, а не из рендера этой страницы** |

### Apple — документация API

| # | Источник | Тип | Что взято |
|---|---|---|---|
| S5 | [Adopting Liquid Glass (Technology Overview)](https://developer.apple.com/documentation/TechnologyOverviews/adopting-liquid-glass) | **Primary (Apple, полный текст)** | Определение материала и функционального слоя; **рекомендация убирать свои фоны** под bars (иначе мешают scroll edge effect); тест с accessibility-настройками; **«Avoid overusing Liquid Glass effects… Limit these effects to the most important functional elements»**; controls coming alive; concentricity API; scroll edge effect API; **«Combine custom Liquid Glass effects to improve rendering performance… GlassEffectContainer»**; перечисление API (SwiftUI/UIKit/AppKit) |
| S6 | [Applying Liquid Glass to custom views](https://developer.apple.com/documentation/SwiftUI/Applying-Liquid-Glass-to-custom-views) | **Primary (Apple, полный текст)** | Определение («blurs content behind it, reflects color and light of surrounding content, and reacts to touch and pointer interactions in real time»); **sampling region больше себя**; **glass не может семплить glass**; `GlassEffectContainer` и shared region; **spacing контейнера управляет слиянием** (и опасность `spacing > interior spacing`); `glassEffectUnion`; `glassEffectID` + `matchedGeometry`/`materialize`; **performance: «too many containers… degrade performance»** |
| S7 | [SwiftUI — `glassEffect(_:in:)`](https://developer.apple.com/documentation/swiftui/view/glasseffect(_:in:)) | **Primary (Apple API)** | **Двухслойная модель**: «Renders a shape anchored **behind** a view» + «Applies the **foreground effects** over a view»; дефолт `.regular` + `Capsule`; «anchors the Liquid Glass to a view’s **bounds**… includes the padding» |
| S8 | [SwiftUI — `GlassEffectContainer`](https://developer.apple.com/documentation/swiftui/glasseffectcontainer) | **Primary (Apple API)** | «combines multiple Liquid Glass shapes into a single shape that can morph»; «SwiftUI **renders the effects together**, improving rendering performance»; слияние путей при сближении; зависимость от `spacing` |
| S9 | [SwiftUI — `Glass`](https://developer.apple.com/documentation/swiftui/glass) | **Primary (Apple API)** | Варианты `.regular`, `.clear`, `.identity`; `interactive(_:)`; `tint(_:)`; `.identity` = «content remains unaffected as if no glass effect was applied» |
| S10 | [UIKit — `UIGlassEffect`](https://developer.apple.com/documentation/uikit/uiglasseffect) | **Primary (Apple API)** | Наследует `UIVisualEffect`; `init(style:)`; `isInteractive`; `tintColor`; `Style` (regular/clear) |

### Web — первичные

| # | Источник | Тип | Что взято |
|---|---|---|---|
| S11 | [MDN — `backdrop-filter`](https://developer.mozilla.org/en-US/docs/Web/CSS/backdrop-filter) | **Primary (Web, полный текст)** | **Baseline 2024**; полный список filter-функций (`blur, brightness, contrast, drop-shadow, grayscale, hue-rotate, invert, opacity, saturate, sepia`); поддержка `url(#svgFilter)` и комбинаций; **правило backdrop root** (6 условий + `<html>`) и его практическое следствие для `opacity: 0.9`; спецификация Filter Effects Module Level 2 |
| S13 | [MDN — `prefers-reduced-transparency`](https://developer.mozilla.org/en-US/docs/Web/CSS/Reference/At-rules/@media/prefers-reduced-transparency) | **Primary (Web, полный текст)** | Статус **Limited availability** (не Baseline) + **Experimental**; синтаксис `no-preference` / `reduce`; путь к системной настройке в Windows/macOS/iOS; спецификация Media Queries Level 5 (черновик) |
| S14 | [Web Platform Features Dashboard — `prefers-reduced-transparency`](https://webstatus.dev/features/prefers-reduced-transparency) | **Primary (vendor data, Chrome team)** | Поддержка **Chrome 119 (31.10.2023)** и **Edge 119 (02.11.2023)**; WPT stable — chrome/edge 1, firefox/safari 0; baseline `limited`; **daily usage ≈ 0,05 %**; standards-position Apple с оговоркой по privacy |

### Runtime

| # | Источник | Тип | Что взято |
|---|---|---|---|
| S12 | WebView2 (Microsoft Edge) — используется Tauri 2 на Windows | **Runtime** | Chromium-based WebView; обеспечивает поддержку `backdrop-filter` (S11), WebGL 1/2, CSS/SVG-фильтров, а также `prefers-reduced-transparency` (S14: Chrome/Edge ≥ 119). **Ограничение:** WebGPU на целевых Windows-машинах **не гарантирован** — поэтому Tier 3 не должен на него полагаться |

### Чего сознательно нет

- **Нет** сторонних разборов/блог-разборов Liquid Glass: все оптические утверждения опираются на S1/S2/S3, а веб-ограничения — на S11. Сторонние «реконструкции шейдера Apple» сознательно не использовались, поскольку это не первичные источники и они противоречат требованию не выдавать гипотезы за факты.
- **Нет** `scrollEdgeEffectStyle` в источниках: точный URL документации вернул 404 при проверке, поэтому поведение scroll edge описано **только** по транскриптам S1/S3 (где оно раскрыто подробно), без ссылки на неподтверждённую страницу.
- **Нет** страницы `NSGlassEffectView` как отдельного источника: она упомянута в S5, но отдельно не загружалась, поэтому утверждения о ней в документе не делаются.

---

## 21. Research Conclusion

### 1. Из каких компонентов фактически состоит качественный Liquid Glass effect

1. **Lensing (преломление/концентрирование света)** — ядро материала; отличает стекло от матового рассеивания.
2. **Backdrop sampling + адаптация слоёв к фону** — каждый слой непрерывно подстраивается под то, что под ним.
3. **Blur/рассеивание, масштабируемое размером** — крупный элемент = более толстое стекло.
4. **Адаптивная тень** — плотнее над текстом, легче над сплошным светлым фоном.
5. **Edge/rim/specular, реагирующие на геометрию** — задают силуэт и выпуклость.
6. **Адаптивная ожупка** — hue/brightness/saturation зависят от яркости подложки.
7. **Foreground vibrancy** — текст/иконки зеркалят полярность материала.
8. **Интерактивный отклик** — scale/bounce/shimmer + свечение изнутри от курсора.
9. **Materialize через модуляцию преломления** — не fade.
10. **Scroll edge effects** — растворяют контент, поднимая материал визуально.
11. **Recede при потере фокуса** — визуально отступает.
12. **Shared container** — соседние элементы делят sampling region и сливаются.

### 2. Обязательные и опциональные

**Обязательны (без них материал не работает):** геометрия с concentricity, backdrop sampling, blur, адаптация тона/тени к подложке, edge-обработка, foreground-контраст, shared-слой для групп, accessibility-тиры.
**Опциональны:** преломление (lensing) — самый дорогой и самый желанный, но не обязательный; specular повышенной точности; сложный морфинг форм; `feTurbulence`-подобная микротекстура.

### 3. Что воспроизводится обычным Web/CSS

Blur (`backdrop-filter`), tint (`color-mix`/переменные), rim и specular (градиенты + `mask` + `mix-blend-mode`), inner glow от курсора (радиальный градиент), адаптивная тень, scroll edge (маска), scale/bounce, materialize, recede по фокусу окна, concentricity (арифметика), foreground-контраст, shared-контейнер (один слой на группу), accessibility-тиры. **То есть практически весь «характер» материала достижим в CSS.**

### 4. Где нужен SVG / WebGL / WebGPU

- **SVG:** только статические/простые вещи (шейп, паттерн, `feColorMatrix`). `feDisplacementMap` теоретически даёт distortion фона, но не даёт screen-space семплинга реального окна и дорог при анимации → **не рекомендуется**.
- **WebGL:** единственный практичный путь к настоящему преломлению (screen-space displacement), к зависимости силы преломления от размера и к адаптации в реальном времени.
- **WebGPU:** то же, современнее; **не гарантирован в WebView2 на всех Windows**, поэтому не может быть обязательным.

### 5. Главные performance bottlenecks

1. **Много независимых перекрывающихся `backdrop-filter`** — самый дорогой паттерн.
2. **Анимируемые фильтры** (blur, SVG) в цикле.
3. **WebGL-пасс на большой области.**
4. **Большой статический blur на большой области.**
5. **Скролл-контент под стеклом** (backdrop меняется каждый кадр) — в проекте это 8 скролл-контейнеров.
6. Много мелких независимых glass-элементов.
Дешёвые и безопасные: градиенты, маски, тени, `border-radius`, `transform`.

### 6. Ограничения конкретно VoidLauncher (Tauri 2 / Windows / WebView2)

- **WebView2 (Chromium)** — `backdrop-filter` доступен (Baseline 2024), WebGL 1/2 доступен, **WebGPU не гарантирован** → Tier 3 только опционально и с fallback.
- **Все 4 темы тёмные** (`colorScheme = 'dark'`) → абсолютное переключение light/dark неприменимо; нужна **относительная** адаптация к яркости подложки.
- **Темы `blueprint` и `ember` используют непрозрачные поверхности** (`#1A1D1F`, `#1A1816`) → под ними нет бэкдропа, материал деградирует в «плоский».
- **`backdrop root` (MDN)** — `opacity < 1`/`mask`/`clip-path`/`mix-blend-mode`/`filter` у предков оборвут эффект.
- **8 скролл-контейнеров** — backdrop пересчитывается при скролле; нужен scroll edge маской и минимум анимированных blur.
- **`.modal-overlay` = `rgba(0,0,0,0.7)`** → glass-on-scrim-glass нарушит запрет.
- **`decorations: false` + `-webkit-app-region: drag`** на titlebar → проверить hit-testing при pointer-реакциях материала.
- **`prefers-reduced-transparency` отсутствует** (есть только `reduced-motion`) → добавить, **плюс** собственный переключатель прозрачности (§13.3).
- **CSP** задан, но CSS/SVG/WebGL-подходы им не ограничены (CSP влияет на загрузку ресурсов и `connect-src`, не на локальные фильтры).
- **Дизайн-система уже готова**: `--surface-glass*`, шкала `--radius-*` (совпадает с concentricity), `--transition-*` (150/250 ms), `prefers-reduced-motion`. Расширять, не дублировать.

### 7. Общий подход при будущей реализации

1. **Цель — Tier 2.** Tier 1 — fallback и проверка приемлемости. Tier 3 — точечно и только при доказанной необходимости.
2. **Точечное применение:** titlebar, sidebar, компактные тулбары, оверлеи. **Не** content layer (списки, логи, таблицы).
3. **Один blur-слой на логическую группу** — прямая трансляция `GlassEffectContainer`, главный рычаг производительности.
4. **Инвестировать в дешёвые слои сначала** (rim, glow, tint, адаптивная тень, scroll edge, foreground-контраст, recede), а не в дорогой lensing: они дают большую часть «характера» при почти нулевой цене.
5. **Адаптация относительная** (по яркости подложки), поскольку тема тёмная; не реализовывать переключение light/dark.
6. **Materialize без fade**, morph на месте (расширение формы, а не исчезновение+появление).
7. **Accessibility by default:** собственный переключатель прозрачности + `prefers-reduced-transparency` (уважение к системе) + существующий `prefers-reduced-motion` (расширить) — см. §13.3.
8. **Feature-query tiers** (`@supports`) + ручной quality-флаг для принудительного упрощения.
9. **Профилировать до утверждения «дёшево».** Никаких чисел без измерений.
10. **Приватные ключи/секреты/CSP/SSRF-allowlist не трогать** — задача чисто визуальная и не должна затрагивать безопасность (правило 6 из `PROJECT_NOTEBOOK.md`).
