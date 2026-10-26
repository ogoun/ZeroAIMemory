---
tags: [frontend, performance, web-vitals, audit]
---
# Storefront frontend performance audit

The storefront's search and product pages had been getting slower for about eighteen months, slowly enough that no single release looked guilty. Field data showed the 75th percentile Largest Contentful Paint on mobile at 4.1 seconds for product pages and 3.6 seconds for search results, both well above the 2.5 seconds that counts as good, and Interaction to Next Paint on search results was poor on mid-range Android phones. This audit was a six-week effort by two frontend engineers and one platform engineer to find out where the time went and fix the biggest problems. The document lists findings in the order we found them, then the fixes and their measured effect in field data.

#### Method

##### Lab and field data

Lab measurements, taken with a fixed device profile and network throttling, are reproducible and good for comparing changes, but they do not tell you what real users experience. Field data from real user monitoring tells you what users experience, but it is noisy and slow to react to changes. We used both: lab runs on every candidate fix to decide whether it helped at all, and field data over at least a week after shipping to decide whether it helped users. Our real user monitoring collects the core web vitals with attribution, meaning it records not just the value of each metric but also which element was the largest contentful paint and which interaction was slowest, along with the device class and connection type. That attribution data turned out to be the most useful input of the whole audit.

###### Segmenting by device

Averages across devices hide almost everything. When we split field data by device class, high-end phones and desktops were already fast; nearly all of the poor experiences came from mid-range and low-end Android devices, which make up about forty percent of our mobile traffic in some regions. Every lab run from then on used a profile that emulated a mid-range Android device with a four-times CPU slowdown, rather than the default profile of a fast laptop.

#### Findings

##### Largest Contentful Paint on product pages

On product pages the largest contentful element was almost always the main product image. The attribution data broke LCP into four parts: time to first byte, the delay before the image request started, the time to download the image, and the delay between download and rendering. Time to first byte was reasonable at around 600 ms on mobile. The download itself was also reasonable. The large part was the delay before the image request started: on median devices, the browser did not begin downloading the main image until about 1.8 seconds after the response started arriving.

###### Why the image was discovered late

The cause was a combination of two decisions made at different times by different teams. The product page had been migrated to client-side rendering of the image gallery component two years earlier, so the main image's URL was not in the initial HTML at all; it only appeared once the JavaScript bundle had downloaded, parsed and executed and the gallery component had rendered. Separately, the image used a lazy-loading attribute set by a shared image component that applied lazy loading to every image by default, including the one image on the page that should be loaded eagerly. Either problem alone would have delayed the image; together they delayed it by more than a second and a half. The fix was to render the main image's markup on the server, with eager loading and a high fetch priority, and to keep the rest of the gallery client-rendered. Example of the markup for the main image:

```html
<img src="/img/p/123/main-800.webp"
     srcset="/img/p/123/main-400.webp 400w, /img/p/123/main-800.webp 800w"
     sizes="(max-width: 600px) 100vw, 50vw"
     fetchpriority="high" loading="eager" decoding="async"
     width="800" height="800" alt="">
```

##### JavaScript cost

The product page shipped about 780 KB of compressed JavaScript on first load, which is over 3 MB uncompressed, and on a mid-range phone parsing and executing it took around two seconds of main thread time. A bundle analysis showed that the biggest contributors were not our own code. A date formatting library with all locales included accounted for about 90 KB, a charting library used only on the reviews tab accounted for about 140 KB, and three different versions of a utility library were bundled because different internal packages depended on different versions. The fixes were mechanical: replace the date library with the platform's built-in internationalization API, load the charting library only when the reviews tab is opened, and deduplicate the utility library by aligning dependency versions. Run the bundle analyzer on a production build with:

```sh
npm run build -- --stats && npx source-map-explorer dist/assets/*.js --html report.html
```

###### Third-party scripts

Third-party scripts were the second large source of main thread work. The page loaded a tag manager that in turn loaded eleven scripts: analytics, two advertising pixels, a session replay tool, an A/B testing script, a chat widget and several others. The A/B testing script was loaded synchronously in the head, because it needed to run before the page rendered to avoid flicker between variants, and it blocked rendering for about 300 ms on mid-range devices. We moved experiments that change page layout to server-side assignment, which made the synchronous client script unnecessary for them, and loaded the remaining third-party scripts after the page became interactive. The session replay tool was the most expensive single script, and after a discussion with the product team it was limited to a one percent sample of sessions instead of all of them.

##### Interaction to Next Paint on search results

On the search results page the problem was responsiveness rather than loading. The slowest interactions were typing in the search box and clicking a filter checkbox. Typing triggered a suggestions request on every keystroke and re-rendered the whole results list when suggestions arrived, because the suggestions state lived in the same top-level store as the results. On a mid-range phone, a single keystroke could take over 400 ms from key press to the next frame. The fix was twofold: suggestions state moved into the search box component so that suggestions updates no longer re-rendered the results, and suggestion requests were debounced to 150 ms after the last keystroke.

###### Filter clicks

Clicking a filter checkbox updated the URL, fetched new results and re-rendered the result grid of up to sixty product tiles. The fetch is asynchronous and does not block the interaction, but before fetching, the code synchronously recomputed facet counts for all filters from the current result set, which took about 200 ms on a mid-range device for large categories. That work did not need to happen before the next paint at all. We changed the click handler to update the checkbox state immediately, yield to the browser so that the checked state paints, and then perform the recomputation and fetch. Example of the pattern we used to yield:

```js
async function onFilterChange(filter, checked) {
  setChecked(filter, checked);          // visible feedback first
  await new Promise(r => setTimeout(r, 0));
  recomputeFacets();
  fetchResults();
}
```

##### Layout shifts

Cumulative Layout Shift was not the main complaint, but the attribution data showed two recurring shifts that were easy to fix once we saw them. The first came from the promotional banner at the top of product pages, which was injected by a client-side script after the page had rendered and pushed the whole page down by about ninety pixels. Users who had started reading or were about to tap a button found the page jumping under their fingers, and the session replay sample showed several mis-taps on the "add to cart" button that landed on the size selector instead. The banner now has reserved space in the server-rendered markup, with a fixed height that matches the banner's design, and the script only fills it in. The second shift came from web fonts: the product title used a custom font that loaded late, and when it arrived, the title reflowed to a different width, sometimes moving to two lines. We added font metric overrides for the fallback font so that it occupies almost exactly the same space as the custom font, which made the swap visually minor.

###### Fonts and preloading

While looking at fonts we also found that the storefront loaded six font files on every page, two weights of the custom font in three formats, although every browser we support only ever used one format. Removing the unused formats did not change the number of requests a browser made, since browsers only download the format they choose, but it did simplify the stylesheet. The real improvement came from preloading the one font file used above the fold and subsetting it to the Latin and Cyrillic ranges we actually display, which reduced its size from 96 KB to 31 KB. On a slow 3G profile in the lab, the title rendered in the final font about 400 ms earlier.

#### Server-side improvements

##### Time to first byte

Although time to first byte was not the largest part of LCP, it was the part that affected every page. The page server rendered product pages on every request by calling five backend services in sequence: product details, price, stock, reviews summary and recommendations. Three of those calls were independent and could run in parallel; making them parallel saved about 180 ms at the median. Recommendations are not needed for the initial render at all, so they moved to a client-side request after load. We also introduced short-lived caching of rendered HTML fragments for the parts of the page that do not depend on the user, such as product details and the reviews summary, with a cache lifetime of sixty seconds and invalidation on product updates.

###### Compression and protocols

A smaller but surprising finding was that responses from the page server were compressed with gzip at a low compression level, while the CDN supported brotli for static assets but passed dynamic HTML through uncompressed from the origin and recompressed it with gzip. Enabling brotli on the origin for HTML and configuring the CDN to keep the origin's encoding reduced HTML transfer size by about twenty percent. The effect on LCP was small, around 40 ms on slow connections, but it cost nothing.

#### Results in field data

After all fixes had been in production for three weeks, the 75th percentile LCP on mobile product pages dropped from 4.1 to 2.3 seconds, and on search results from 3.6 to 2.2 seconds. Interaction to Next Paint on search results at the 75th percentile went from 380 ms to 160 ms on mobile. The largest single improvement came from server-rendering the main product image with high priority, which alone reduced product page LCP by about 1.1 seconds. Conversion rate on mobile product pages rose by a little over three percent relative in the weeks after the changes, but several marketing campaigns ran at the same time, so we do not attribute that number to the audit with any confidence.

##### Keeping it fast

Performance regressions came back slowly last time, one small change at a time, so the main outcome of the audit is not the fixes but the guardrails. Every pull request to the storefront now runs a lab test on the mid-range device profile for the product and search pages and comments the change in LCP, total blocking time and bundle size compared to the main branch. Bundle size has a budget per page, and a pull request that exceeds it needs an explicit approval from the performance owners. Field data is reviewed weekly in the frontend team meeting, with the 75th percentile values by page type and device class on one slide, and any page that crosses the "good" thresholds triggers an investigation.

###### Shared components

One lesson applies beyond this audit. Two of the biggest problems, the lazy-loaded main image and the duplicate utility library, came from shared components and shared dependencies whose defaults were reasonable in general and harmful in one important case. The image component now requires an explicit loading mode instead of defaulting to lazy, and the dependency alignment check runs in the shared packages' own build, so that conflicting versions are caught before they reach any application.
