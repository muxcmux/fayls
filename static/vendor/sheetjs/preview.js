(() => {
  const status = document.getElementById("status");
  const tabs = document.getElementById("sheet-tabs");
  const content = document.getElementById("sheet-content");

  function renderSheet(worksheet, name) {
    if (!worksheet || !worksheet["!ref"]) {
      const empty = document.createElement("p");
      empty.textContent = "This sheet is empty.";
      return empty;
    }

    const range = XLSX.utils.decode_range(worksheet["!ref"]);
    const mergeStarts = new Map();
    const mergedCells = new Set();

    for (const merge of worksheet["!merges"] || []) {
      mergeStarts.set(`${merge.s.r}:${merge.s.c}`, merge);
      for (let row = merge.s.r; row <= merge.e.r; row++) {
        for (let col = merge.s.c; col <= merge.e.c; col++) {
          if (row !== merge.s.r || col !== merge.s.c) {
            mergedCells.add(`${row}:${col}`);
          }
        }
      }
    }

    const table = document.createElement("table");
    table.setAttribute("aria-label", name);

    const head = table.createTHead().insertRow();
    head.appendChild(document.createElement("th"));
    for (let col = range.s.c; col <= range.e.c; col++) {
      const heading = document.createElement("th");
      heading.scope = "col";
      heading.textContent = XLSX.utils.encode_col(col);
      head.appendChild(heading);
    }

    const body = table.createTBody();
    for (let row = range.s.r; row <= range.e.r; row++) {
      const tr = body.insertRow();
      const heading = document.createElement("th");
      heading.scope = "row";
      heading.textContent = row + 1;
      tr.appendChild(heading);

      for (let col = range.s.c; col <= range.e.c; col++) {
        const key = `${row}:${col}`;
        if (mergedCells.has(key)) continue;

        const td = tr.insertCell();
        const merge = mergeStarts.get(key);
        if (merge) {
          td.rowSpan = merge.e.r - merge.s.r + 1;
          td.colSpan = merge.e.c - merge.s.c + 1;
        }

        const cell = worksheet[XLSX.utils.encode_cell({ r: row, c: col })];
        if (cell) {
          const value = cell.w ?? XLSX.utils.format_cell(cell) ?? cell.v ?? "";
          td.textContent = String(value);
        }
      }
    }

    return table;
  }

  try {
    const base64 = document.getElementById("workbook-data").content.textContent.trim();
    const bytes = Uint8Array.fromBase64
      ? Uint8Array.fromBase64(base64)
      : Uint8Array.from(atob(base64), char => char.charCodeAt(0));
    const workbook = XLSX.read(bytes, { type: "array", cellHTML: false });
    if (!workbook.SheetNames.length) throw new Error("The workbook has no sheets.");

    let activeButton;
    function selectSheet(name, button) {
      if (activeButton) activeButton.setAttribute("aria-pressed", "false");
      button.setAttribute("aria-pressed", "true");
      activeButton = button;
      content.replaceChildren(renderSheet(workbook.Sheets[name], name));
      content.scrollTo(0, 0);
    }

    for (const name of workbook.SheetNames) {
      const button = document.createElement("button");
      button.type = "button";
      button.textContent = name;
      button.setAttribute("aria-pressed", "false");
      button.addEventListener("click", () => selectSheet(name, button));
      tabs.appendChild(button);
    }

    selectSheet(workbook.SheetNames[0], tabs.firstElementChild);
    status.hidden = true;
    tabs.hidden = false;
    content.hidden = false;
  } catch (error) {
    status.textContent = `Could not preview this spreadsheet: ${error.message}`;
  }
})();
