export const note = `fragment notes:

[x] wrote it in rust with grep for search - THIS WAS A BIG DEAL
[ ] create the notational velocity three pane ui
[ ] click a note to show it in the third pane


search logic:

[ ] search by title first
[ ] (pre-search, hopefully): sort by name or date or etc.


stream ui:

[ ] design an "inbox" view
[ ] tagging (separate database?)
[ ] 


bugs:

[ ] fix the class stripping issue, might have to use webpack

https://github.com/emk/electron-test/blob/master/webpack.config.js


====


REQUIREMENTS
if you have the application open, and the keyboard plugged in, it transfers over all the snippets (maybe you should push a button?)

a way to search through all my notes, and add my incoming snippets to existing or new notes














so really there's an "inbox" and an archive






I could make a web-based version
but my primary use case is on the desktop.
so I want to keep track of a folder of text files, sync that other place

====


OR WHAT IF I JUST USED TEXT FILES BECAUSE DATABASES ARE HARD FOR ME

where do I store the metadata?

in the text file:
    pros:
        metadata is colocated with the file
    cons:
        what if someone edits the metadata? then they break everything!
        what do I do if someone just adds a new blank text file? do I try to add metadata?
    
in separate text files per metadata type:
    
    pros:
        files can just be plain text, metadata is always external and additional
        can generate new metadata without having to edit the files at all
        could migrate to a database later if that's easier or faster
        
    cons:
        could be slower because I have to look up stuff?
        
        
in a database:
    pros:
        fast
    cons:
        could cause lock-in
        I don't know how to use databases
        
        
        
TODO:
build a service that lets me search my notes folder from a webbroweser

step 1: make a web app
    [x] 0: input text box
     1: list of results
    [x] DIDN'T HAVE TO2: SOLVE CORS OMG
    
step 2: make a server
    [x] 0: make a hello world server
    [x]1: respond to json requests (simply)
    [x] 2: respond to json requests (more complicated)
    [] 3: switch over to grpc because of course
    
    


=====



default action is to add a new note
it's tagged "uncategorized", stamped with a date in the toml, and given a uuid

these "fragments" are composed into "views" which are plain text documents you can edit. when you make changes, the diffs become a new fragment

you can't edit a fragment, but how do you edit a note that's built of combined fragments?


WHAT I WANT TO DO
write notes at any time
categorize them later as notes, bookmarks, todos, etc.
combine them into topics with a sort of wiki like interface of interlinking
add "attachments" of any type: could be links, photos, bookmarks, or other notes
edit notes in any text editor
search messages, tweets, emails, and a crawled version of bookmarks


===

It all has to be encrypted!

So maybe the standard text edit view is just "view" that exposes the notes as a virtual file system

So the database can be anything!

===

Requirements

You can host it on digital ocean for $10 a month
Backs up to Dropbox and or Google drive
You can login into new services and generate a key for them to use your db

====

Would be cool to have a community maintained list of types that services can compose from

====

Client does encryption / decryption, server never knows anything


[custom client] (javascript or wasm)
client library (wasm / grpc)

server library (grpc)
    [custom server logic] (wasm or rust)
lmdb

can you have a server that only sees encrypted stuff?

====

You write a file that lists the types of data you want to store

And then you can use that data from the client and store it to the server.

It's basically just a stuct definition, with substructs

Keys in the database have a type, an app id, a date, and a uuid


===

database lmdb alternative:
https://www.reddit.com/r/rust/comments/46uvik/new_crate_sanakirja_a_keyvalue_dictionary/


===

how to use an arc mutext database maybe:
https://github.com/yoshuawuyts/playground-http-speedrun/blob/master/http_02/src/main.rs


===

grpc web

https://github.com/improbable-eng/grpc-web/tree/master/go/grpcwebproxy
https://github.com/improbable-eng/grpc-web

https://github.com/stepancheg/grpc-rust`;
