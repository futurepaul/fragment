"use strict";
Object.defineProperty(exports, "__esModule", { value: true });
exports.note = `Creators and distributors 

By creating a distributed, edge-first internet, we could destroy the inherent advantage that centralized distributors have and give that value to the creators who can now invest more in building better stuff

Amazon for shopping
Google for search 
Facebook for community
YouTube and Netflix for video entertainment
Seamless

<em>test</em>


Have to make decentralization the advantage, not an ineffiency. As long as centralization is more efficient, it wins. Once decentralization is more efficient, centralized control becomes a cost.


Content addressable is big


Food
Shelter
Community
Education
Work


Anything we can’t do ourselves or with a tool that we buy we have to pay or ask someone else to do it for us


Rely on recommendations from a trusted network for verifying quality 

I want wine. It trust three people’s opinions on wine. My network is searched for someone who has a flavor profile that matches mine. And I can also search friends of friends.

I pick the content-addressed wine I want.

Now it’s delivered to me from the nearest location. Or I can go there to pick it up.

The delivery service and the store I Buy from can be web-of-trust sorted as well.

I can also sort my initial search by estimated time to arrive.

I can choose to tip the web of trust people I used. Some trust services can have an upfront fee to allow access (wirecutter)


IPFS solves most of the mars internet problems


You can always pull up the app to check for outstanding requests you can serve. Maybe you can pick up that bottle of wine on your commute home and drop it off at that guys apartment. You were already making the trip and now you got paid $10 for making the trip.


Google search is a fallback for searching through the stories from your trusted sources and the delicious links from your friends

Why does amazon use ASN instead of UPCs


Network layer: how do you solve DNS


What can be decentralized without the slowdown of consensus?


When you sell something to a reseller they get an ownership token


=== Product ideas ===

app where you scan an item you own and rate it
app that lets me search products my friends (and trusted third parties) have recommended
also ties into the movie match-making service
amazon replacement
    people can be product sellers
    distributors
    retail stores
    raters
    deliverers
    buyers
alternative to google knowledge graph
alternative to google news search
upc alternative
asn alternative (for tracking a shipment?)
can you create dewey deciman system isbn for news topics? (story stream 2.0)
techmeme 2.0 (add this source to “trusted” list and give it a score of how much you think it’s trusted. now it shows up ranked with other news sources you’ve ranked (unvetted is 50% unless it has a bunch of negative scores from your friends)
“show me more stories on this topic” “tag this story with a topic”
I don’t want to “share” a story, I want to recommend it to people in my network who are interested in certain topics

base it initially on twitter shares from people you follow and the people they follow

a classification system for news

tag a story based on the:
    dewey decimal system number
    publisher
    author
    people of interest mentioned (primary and secondary?)
    companies mentioned (p and s)
    products mentioned (p and s)

incentivize people to tag and sanitize data for the network

=== edge computing in general ===
who “owns” the computation?
are you colocating amazon infrastructure, or buying your own hardware

=== peer to peer in general ===

kevin bacon “problem” or kevin bacon “advantage”???

====

Bgp lookup table
Bgp routing
Bgp addresses
cognitive radio (for 5g)
all about solving the traveling salesman problem
wefi.net in spain

edge cloud server


==== Noooooodes ====

https://github.com/treescale/treescale

http://christophermeiklejohn.com/lasp/erlang/2015/10/27/tendency.html

https://youtu.be/JHQlA_tB10c
==== COORDINATION OVERHEAD ====




==== examples ====

Sifttery
https://siftery.com

Compared to google shopping

Wirecutter


Build a buyer side business


Subscribe to data from Paul 

@futurepaul/tweets or @futurepaul/varbs 

Subscribe to news topics from verge (exactly like rss

Or subscribe to verge stories that Paul likes 


Request bids to supply a product by ID

Standing requests


You can be an end user, matchmaker, or supplier. Often all three.

People will also sell filters you can apply (like a spam filter for instance)
Local crawling


A distributed search engine. You crawl everything you browse but shallowly. Then you can add trusted peers (duck duck go, google, your buddy Kyle)

By default you publish every link you’ve clicked on as a search result??? Omg privacy.

It’s off by default. You start a search session and make it obvious you’re using it.

Make it work for local search and then just publish topics you’re good at. Can give out a key to a friend, and can revoke that at any time (in case they share it or you hate them now)

=======

Could you wc twitter?

Name it after that name for number of people you can know

Publish “tktktk”

Private data, shared data, and public data

dunbar://weather/99336

dun://@futurepaul/tweets

if I want @futurepaul’s tweets I ask futurepaul for a hash of his last 30 tweets and then get them from him if me or someone close to me doesn’t have them 

====

Use unused space on digital ocean boxes
$6 a month
For two people to be friends you both add each other. You give each other a token that works as a way to look up your current proxy’s IP address



To find public data:
Eight people I’ll share this with, eight people they’ll share it with, etc

OOPS NOT PEOPLE

Name eight pieces of content, each of which name eight pieces of content

To share private data, it’s the same address but it’s been encrypted and I’ve given someone the key.

It’s like breadcrumbs in Hansel and Gretel!

As I search for the thing I’m looking for, I always want to hop out of the network I’m currently in

====

Personas, groups, topics
Nodes can choose which groups and topics they’re willing to host

You create a persona and receive a key that allows you to publish and receive as that persona

You connect to a node that you know serves the group or topic you’re interested in (miller family, or Jordan Peterson)

Now you can publish to a topic (public) or group (private) (or to a topic within a group: millerfam/jpeterson)

And you can choose topics or groups to listen to

To join a group you have to be added to a list of people who can decrypt messages of that group.

All encryption / decryption should happen on the client? Is that possible?

And how does the node not leak who is talking? It probably does.

“Only people who can prove they have the the private key to a whitelisted persona can join a group”

With one-to-one messaging you could just ask the node for the location of the person you want to talk to and then take it from there

You can set up persona proxies for certain topics from other of your personas (so I can tweet and my tweets on certain topics will go to my family)

Can you publish anonymous messages to a topic? Seems easy to abuse. Persona whitelisting really cuts down on spam.

Stage 1: don’t have a wide network (no buying wine bottles)
Stage 2: use networks of trust to bootstrap the wide network 

People can publish types to the “types” topic (reserved words for topics?) and then can publish typed messages to topics and groups. Can use this to upgrade the protocol?

A group has agreed to a certain list of acceptable types. A node can also list the types it doesn’t like maybe.

Topics can have an optional type. 240 character UTF-8 string is the default type. But you can publish an image based on the image type you’ve subscribed to to jordanpeterson/image

Default types: string, image, movie, searchdb, productrequest, productprovider, store, producer, restaurant, servicerequest, serviceprovider, keyvaluedb

Nodes can subscribe to popular type definitions, and if they don’t want to upgrade to a new type they can add a translator

You should really trust the node you’re trusting 

Oh shit do nodes mean this doesn’t work offline?

Scan each other’s QR codes to create a make-do network in case of emergencies

You can only talk to someone when you know where they are and you do that through QR codes IRL or by them telling the node where they are 

If there’s no node and no backup nodes, how does a group find a new node?

===
Faith hope love, about these things there is no law
===

Tktk


====

I FOUND THE TECH
https://github.com/ssbc/scuttlebot

https://coolguy.website/writing/the-future-will-be-technical/dinner-party.html

http://milinda.pathirage.org/kappa-architecture.com/

===

“The man of system”

People who design distributed systems as a single machine are describing the how AND the why

Master and his emissary (econtalk!)
If it’s all one system, we are all the emissaries of that system

The only fix is to have more makers, who generate a system that is the interaction of autonomous systems 

Tragedy is when people think they can know it all and control it all. Tragedy comes from hubris.

Let me show you how I can hurt someone. Not by saying something that’s definitively harmful, but contextually harmful.

Harm and freedom. Cutting off one is like cutting off one end of a magnet.

“Love is a pure attention to the existence of the other”

By attending to the world mechanically we destroy its meaning

Contract v covenant.
If I have a contract with Facebook listing how not to harm people, I can still harm people

7 type of ambiguity

====

Our programs should be servants not masters

Democracy isn’t a way to determine what’s “true” or “good”

The way to maximize the utility of Facebook to you is to determine what your purpose is. But what if your purpose is at odds with the purpose of the creators of Facebook?

I wish to both absolve and indict Facebook 

A society can’t be designed it has to be lived and built. The embodiment is the thing itself.

“Real conmunism has never been tried”

No, it’s just never been embodied outside of Acts

===

https://github.com/spacejam/sled

COUCHDB IS WHAT I NEEDED THIS WHOLE TIME I THINK

Kinto?
Gundb


===

LOGUX??? (crdt + redux)
https://www.youtube.com/watch?v=3LecnX1hjyw

don't forget about this storage thing: https://github.com/developit/unistore

https://sit.fyi


====

Distribution without decentralization is a cost

====

https://twitter.com/sowelldaily/status/1013305851069820928

Someone once said that a fool can put on his coat better than a wise man can put it on for him. The implications of that undermine most of the agenda of the political left.

====

Far easy to concentrate power than to concentrate knowledge (Sowell)

Consequential knowledge tends to be widely diffuse (sowell)

===

easier app development
https://tryretool.com

The state of things
https://blog.logrocket.com/data-fetching-in-redux-apps-a-100-correct-approach-4d26e21750fc





===

future:

https://github.com/google/xi-editor/blob/e8065a3993b80af0aadbca0e50602125d60e4e38/doc/crdt-details.md

====

Data has a type, but can be tagged with references to pieces of data of other types, and those references are bidirectional

So I can write a text document and tag it that it was inspired by a convo with chris

And when I pull up my Chris contact, it shows a reference to that text document 

The “where” of content doesn’t matter. The content address (IPFS-style) and the encryption key (if applicable). 

===

GDP growth correlates with interpersonal trust bonds

===

There’s nothing special about us. We have a civilization built up but that doesn’t make us individuals any more advanced. We don’t vote food into existence. We don’t get iPhones because we deserve them more. So how do gain civilization and how do you lose one?

===

Are we decentralized yet?


===

Cos theorem about efficiency 


`;
//# sourceMappingURL=demo_note.js.map