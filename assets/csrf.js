document.addEventListener("DOMContentLoaded", function () {
  var meta = document.querySelector('meta[name="csrf-token"]');
  var token = meta ? meta.getAttribute("content") : "";
  document.body.addEventListener("htmx:configRequest", function (evt) {
    evt.detail.headers["X-CSRF-Token"] = token;
  });
});
