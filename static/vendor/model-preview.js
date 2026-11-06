const viewer = document.getElementById("model");
const status = document.getElementById("status");
const source = viewer.dataset.src;
let objectUrl;

function showError(message) {
  status.textContent = `Could not preview this 3D model: ${message}`;
  status.hidden = false;
}

viewer.addEventListener("load", () => {
  status.hidden = true;
  if (objectUrl) URL.revokeObjectURL(objectUrl);
  objectUrl = undefined;
});

viewer.addEventListener("error", () => {
  showError("the model could not be loaded.");
});

window.addEventListener("pagehide", () => {
  if (objectUrl) URL.revokeObjectURL(objectUrl);
});

async function fetchFile(url) {
  const response = await fetch(url);
  if (!response.ok) throw new Error(`file request failed (${response.status}).`);
  return response;
}

function linkedFileUrl(uri) {
  if (uri.startsWith("data:") || uri.startsWith("blob:")) return uri;

  const previewUrl = new URL(source, location.href);
  const modelPath = previewUrl.searchParams.get("path");
  const encodedPath = modelPath.split("/").map(encodeURIComponent).join("/");
  const resolved = new URL(uri, new URL(encodedPath, location.origin));

  if (resolved.origin !== location.origin) return resolved.href;

  previewUrl.searchParams.set("path", decodeURIComponent(resolved.pathname));
  return previewUrl.href;
}

async function loadGltf() {
  const gltf = await (await fetchFile(source)).json();

  // A blob URL has no model directory. Rewrite linked assets to the existing
  // authenticated preview endpoint before giving the glTF to model-viewer.
  for (const entry of [...(gltf.buffers || []), ...(gltf.images || [])]) {
    if (entry.uri) entry.uri = linkedFileUrl(entry.uri);
  }

  return new Blob([JSON.stringify(gltf)], { type: "model/gltf+json" });
}

async function convertGeometry(format) {
  const THREE = await import("three");
  const [{ GLTFExporter }, { PLYLoader }, { STLLoader }] = await Promise.all([
    import("three/addons/exporters/GLTFExporter.js"),
    import("three/addons/loaders/PLYLoader.js"),
    import("three/addons/loaders/STLLoader.js"),
  ]);

  const data = await (await fetchFile(source)).arrayBuffer();
  const geometry = format === "stl"
    ? new STLLoader().parse(data)
    : new PLYLoader().parse(data);

  if (!geometry.getAttribute("position")?.count) {
    throw new Error("the file contains no geometry.");
  }

  const vertexColors = geometry.hasAttribute("color");
  let object;

  if (format === "ply" && !geometry.index) {
    object = new THREE.Points(geometry, new THREE.PointsMaterial({
      color: 0xffffff,
      vertexColors,
      size: 3,
    }));
  } else {
    if (!geometry.hasAttribute("normal")) geometry.computeVertexNormals();
    object = new THREE.Mesh(geometry, new THREE.MeshStandardMaterial({
      color: 0xffffff,
      vertexColors,
      side: THREE.DoubleSide,
      roughness: 0.8,
    }));
  }

  const glb = await new GLTFExporter().parseAsync(object, { binary: true });
  return new Blob([glb], { type: "model/gltf-binary" });
}

try {
  switch (viewer.dataset.format) {
    case "glb":
      viewer.src = source;
      break;
    case "gltf":
    case "ply":
    case "stl": {
      const blob = viewer.dataset.format === "gltf"
        ? await loadGltf()
        : await convertGeometry(viewer.dataset.format);
      objectUrl = URL.createObjectURL(blob);
      viewer.src = objectUrl;
      break;
    }
  }
} catch (error) {
  showError(error.message || "an unexpected error occurred.");
}
