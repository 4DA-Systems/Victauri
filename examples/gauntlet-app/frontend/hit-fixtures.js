// Hit-test fixtures for the gauntlet battery: which elements a click can reach, on this engine.
// `run()` shows them over the page, clicks each through the bridge, removes them and returns one
// row per fixture, so the battery leaves the shared page as it found it.

(function () {
  const SVG = (id) =>
    `<svg width="60" height="60"><circle id="${id}" cx="30" cy="30" r="25" fill="none" stroke="black" stroke-width="4"></circle></svg>`;
  const WRAPPED = (id) =>
    `<a id="${id}" href="#x" style="line-height:30px;font-size:12px">wrapped link text across two lines</a>`;
  const GIF = "data:image/gif;base64,R0lGODlhAQABAAAAACH5BAEKAAEALAAAAAABAAEAAAICTAEAOw==";

  // [id, what a user can do with it, markup]
  const FIXTURES = [
    // A user cannot see or reach these.
    ["hf_svg_border_closed", "covered", `<div style="height:0;overflow:hidden;border:1px solid #888;width:100px">${SVG("hf_svg_border_closed")}</div>`],
    ["hf_svg_padding_closed", "covered", `<div style="height:0;overflow:hidden;padding:3px;width:100px">${SVG("hf_svg_padding_closed")}</div>`],
    ["hf_svg_1px", "covered", `<div style="max-height:1px;overflow:hidden;width:100px">${SVG("hf_svg_1px")}</div>`],
    ["hf_svg_sronly", "covered", `<div class="hf-sr">${SVG("hf_svg_sronly")}</div>`],
    ["hf_svg_clip_path", "covered", `<div style="clip-path:inset(50%);width:100px;height:80px">${SVG("hf_svg_clip_path")}</div>`],
    ["hf_link_closed", "covered", `<div style="height:0;overflow:hidden;border:1px solid #888;width:80px">${WRAPPED("hf_link_closed")}</div>`],
    ["hf_link_sronly", "covered", `<div class="hf-sr">${WRAPPED("hf_link_sronly")}</div>`],
    ["hf_accordion", "covered", `<div style="height:0;overflow:hidden"><button id="hf_accordion" style="width:100px;height:40px">x</button></div>`],
    ["hf_sidebar", "covered", `<div style="width:0;overflow:hidden"><button id="hf_sidebar" style="width:100px;height:40px">x</button></div>`],
    ["hf_button_sronly", "covered", `<button id="hf_button_sronly" class="hf-sr" style="white-space:nowrap">x</button>`],
    ["hf_under_after", "covered", `<div class="hf-over"><button id="hf_under_after" style="width:100px;height:40px">x</button></div>`],
    ["hf_link_under_after", "covered", `<div class="hf-over" style="width:90px;font-size:12px;line-height:20px"><a id="hf_link_under_after" href="#x">wrapped link text across lines</a></div>`],
    ["hf_svg_under_after", "covered", `<div class="hf-over">${SVG("hf_svg_under_after")}</div>`],
    ["hf_sibling_cover", "covered", `<div style="position:relative;width:110px;height:60px"><button id="hf_sibling_cover" style="width:100px;height:40px">x</button><div style="position:absolute;inset:0;background:red"></div></div>`],
    ["hf_sibling_centre", "covered", `<div style="position:relative;width:100px;height:40px"><button id="hf_sibling_centre" style="width:100px;height:40px">x</button><div style="position:absolute;left:30px;top:10px;width:40px;height:20px;background:red"></div></div>`],
    ["hf_behind", "covered", `<div style="position:relative;z-index:0;width:110px"><div style="background:#eee;width:110px;height:60px"><button id="hf_behind" style="position:relative;z-index:-1;width:100px;height:40px">x</button></div></div>`],
    ["hf_fixed_clip", "covered", `<div style="position:relative;width:60px;height:60px;overflow:clip"><div style="height:200px"></div><button id="hf_fixed_clip" style="width:50px;height:40px">x</button></div>`],
    ["hf_closed_in_control", "covered", `<button class="hf-rip" style="width:100px;height:40px"><div style="height:0;overflow:hidden"><span id="hf_closed_in_control">Save</span></div></button>`],
    ["hf_off_in_control", "covered", `<button class="hf-rip" style="width:100px;height:20px;overflow:clip"><span id="hf_off_in_control" style="position:relative;top:60px">Save</span></button>`],
    // A user can operate these.
    ["hf_span_in_button", "ok", `<button class="hf-rip" style="width:100px;height:40px"><span id="hf_span_in_button">Save</span></button>`],
    ["hf_span_in_link", "ok", `<a class="hf-rip" href="#y" style="display:inline-block;width:100px;height:40px"><span id="hf_span_in_link">Open</span></a>`],
    ["hf_text_in_card", "ok", `<a class="hf-rip" href="#z" style="display:block;width:100px;height:60px"><div id="hf_text_in_card" style="padding:5px">Card</div></a>`],
    ["hf_empty_a_abs", "ok", `<a id="hf_empty_a_abs" href="#a"><span style="position:absolute;left:10px;top:30px">abs text</span></a>`],
    ["hf_empty_a_float", "ok", `<a id="hf_empty_a_float" href="#b"><img alt="" style="float:right;width:40px;height:40px;background:#ccc" src="${GIF}"></a>`],
    ["hf_donut", "ok", `<div style="width:100px;height:80px"><button id="hf_donut" style="width:80px;height:80px;background:#88f;clip-path:path(evenodd,'M0 0H80V80H0Z M30 30H50V50H30Z')">x</button></div>`],
    ["hf_clip_half", "ok", `<div style="width:40px;height:40px;overflow:clip"><button id="hf_clip_half" style="width:100px;height:40px">xxxxx</button></div>`],
    ["hf_plain_inline", "ok", `<div><a id="hf_plain_inline" href="#p">one line</a></div>`],
    ["hf_row", "ok", `<table><tr id="hf_row"><td>a</td><td>b</td></tr></table>`],
    ["hf_stroke", "ok", `<div>${SVG("hf_stroke")}</div>`],
    ["hf_ring_with_dot", "ok", `<svg width="60" height="60"><circle id="hf_ring_with_dot" cx="30" cy="30" r="25" fill="none" stroke="black" stroke-width="4"></circle><circle cx="30" cy="30" r="8"></circle></svg>`],
    ["hf_group", "ok", `<svg width="100" height="100"><g id="hf_group"><rect x="0" y="0" width="10" height="10"></rect><rect x="90" y="90" width="10" height="10"></rect></g></svg>`],
    ["hf_wrapped", "ok", `<div style="width:80px">${WRAPPED("hf_wrapped")}</div>`],
    ["hf_scrolled_in", "ok", `<div style="height:20px;overflow:hidden;width:110px"><div style="height:60px"></div><button id="hf_scrolled_in" style="height:30px;width:100px">x</button></div>`],
    ["hf_scroller", "ok", `<div style="width:110px;height:50px;overflow:auto"><div style="height:500px"></div><button id="hf_scroller" style="width:100px;height:40px">x</button></div>`],
    ["hf_plain", "ok", `<div style="width:110px;height:60px"><button id="hf_plain" style="width:100px;height:40px">x</button></div>`],
    ["hf_mui", "ok", `<span style="position:relative;display:inline-block;width:40px;height:40px;background:#ccc"><input id="hf_mui" type="checkbox" style="position:absolute;inset:0;opacity:0;width:100%;height:100%;margin:0"></span>`],
    ["hf_labelled", "ok", `<label style="position:relative;display:block;width:100px;height:40px"><input id="hf_labelled" type="checkbox" style="position:absolute;left:10px;top:10px"><span style="position:absolute;inset:0"></span></label>`],
    ["hf_corner_free", "ok", `<div class="hf-hole" style="width:100px;height:40px"><button id="hf_corner_free" style="width:100px;height:40px">x</button></div>`],
  ];

  async function run() {
    const layer = document.createElement("div");
    layer.id = "hf-layer";
    layer.innerHTML = FIXTURES.map(
      (f, i) => `<div class="hf-cell" style="left:${(i % 8) * 120}px;top:${Math.floor(i / 8) * 105}px">${f[2]}</div>`,
    )
      .join("")
      .replaceAll(' style="', ' data-hf-style="');
    // The page CSP refuses inline style attributes, not the CSSOM.
    for (const el of layer.querySelectorAll("[data-hf-style]")) {
      el.style.cssText = el.getAttribute("data-hf-style");
    }
    // Fixture clicks stay here: a link must not move the page.
    layer.addEventListener("click", (e) => e.preventDefault(), true);
    document.body.appendChild(layer);
    const bridge = window.__VICTAURI__;
    const rows = [];
    try {
      for (const [id, want] of FIXTURES) {
        const el = document.getElementById(id);
        const row = { id, want };
        const found = bridge.findElements({ css: "#" + id, include_hidden: true })[0];
        const clicked = await bridge.click(found.ref_id, 150);
        row.got = clicked.ok ? "ok" : clicked.error.indexOf("covered") !== -1 ? "covered" : clicked.error;
        const point = bridge.hitPoint(el, true);
        if (!point.error) {
          const box = el.getBoundingClientRect();
          const hit = document.elementFromPoint(point.x, point.y);
          row.point_hits = !!hit && (hit === el || el.contains(hit));
          row.at_centre = point.x === box.left + box.width / 2 && point.y === box.top + box.height / 2;
        }
        rows.push(row);
      }
    } finally {
      layer.remove();
    }
    return rows;
  }

  // Thin strokes: the point a trusted click would use must be on the stroke in the pixel the click
  // is delivered to (the native side truncates x * devicePixelRatio). The page cannot send an OS
  // click, so it asks the engine what is at that pixel's coordinate, where the event hit-tests.
  function thin() {
    const NS = "http://www.w3.org/2000/svg";
    const layer = document.createElement("div");
    layer.id = "hf-layer";
    const svg = document.createElementNS(NS, "svg");
    svg.setAttribute("width", "700");
    svg.setAttribute("height", "500");
    layer.appendChild(svg);
    document.body.appendChild(layer);
    const ratio = window.devicePixelRatio || 1;
    const out = { ok: 0, missed: 0, refused: 0, first_miss: null };
    try {
      for (let i = 0; i < 120; i++) {
        const c = document.createElementNS(NS, "circle");
        c.setAttribute("cx", String(40 + (i % 10) * 65 + ((0.37 * i) % 1)));
        c.setAttribute("cy", String(40 + Math.floor(i / 10) * 38 + ((0.61 * i) % 1)));
        c.setAttribute("r", String(9 + (i % 9)));
        c.setAttribute("fill", "none");
        c.setAttribute("stroke", "black");
        c.setAttribute("stroke-width", "1");
        svg.appendChild(c);
        const point = window.__VICTAURI__.hitPoint(c, true);
        if (point.error) {
          out.refused++;
        } else {
          const x = Math.trunc(point.x * ratio) / ratio;
          const y = Math.trunc(point.y * ratio) / ratio;
          if (document.elementFromPoint(x, y) === c) out.ok++;
          else {
            out.missed++;
            out.first_miss = out.first_miss || { point, x, y };
          }
        }
        c.remove();
      }
    } finally {
      layer.remove();
    }
    return out;
  }

  // A button in a same-origin frame with the given styles on the frame, and on a wrapper around it.
  // Returns the button's ref, for the trusted click probe. No inline style attributes: see the CSS.
  function frame(frameStyle, wrapperStyle) {
    frameClean();
    const wrapper = document.createElement("div");
    wrapper.id = "hf-frame-wrapper";
    wrapper.style.cssText = "position:fixed;left:40px;top:40px;width:320px;height:320px;z-index:2147483647";
    wrapper.style.cssText += wrapperStyle || "";
    const f = document.createElement("iframe");
    f.style.cssText = "border:0;width:300px;height:300px;background:#fff;" + (frameStyle || "");
    wrapper.appendChild(f);
    document.body.appendChild(wrapper);
    const d = f.contentDocument;
    const b = d.createElement("button");
    b.id = "hf-framed";
    b.textContent = "hf framed button";
    b.style.cssText = "position:absolute;left:100px;top:100px;width:60px;height:40px";
    d.body.appendChild(b);
    return window.__VICTAURI__.findElements({ tag: "button", text: "hf framed button" })[0].ref_id;
  }
  function frameClean() {
    const old = document.getElementById("hf-frame-wrapper");
    if (old) old.remove();
  }
  function supportsZoom() {
    return typeof CSS !== "undefined" && CSS.supports("zoom", "2");
  }

  window.__hitFixtures = { run, thin, frame, frameClean, supportsZoom };
})();
