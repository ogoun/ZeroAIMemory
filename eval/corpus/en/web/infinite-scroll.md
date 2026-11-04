# Infinite scroll in the product listing

The product listing loads more products as the user scrolls instead of showing page numbers.
The listing component owns the cursor returned by the API and appends each new page.

## Triggering the next page

A sentinel element sits after the last product card. An `IntersectionObserver` with a
`rootMargin` of 600 px fires when the sentinel approaches the viewport, and the component
requests the next page with the saved cursor.

## Loading state

While a page is loading the component shows skeleton cards. An error shows an inline "Try
again" button in place of the sentinel; the observer is disconnected until the user clicks it.

## Back navigation

The listing stores the loaded pages and the scroll offset in session storage. Returning from a
product page restores both, so the user lands where they left instead of at the top.

## Accessibility

A "Load more" button remains in the DOM for keyboard and screen reader users and for browsers
with reduced motion settings.
