# Refactor Step 1 Notes

This version performs the lowest-risk separation of the original single-file app:

- `src/index.html` keeps the existing markup and inline event attributes.
- `src/styles.css` contains the extracted CSS from the original `<style>` block.
- `src/app.js` contains the extracted JavaScript from the original `<script>` block.
